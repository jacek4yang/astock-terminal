//! iwencai slider-captcha solver (feature `captcha`).
//!
//! The challenge served by `www.iwencai.com/ac_verification/captcha/` is a
//! **slider puzzle** (captcha_type=4 on `captcha.10jqka.com.cn`), not a text
//! image — verified live 2026-08-22 against widget
//! `s.thsi.cn/js/jsmodule/web/captcha/v1.4/captcha.min.js`, whose bar text
//! is literally "向右拖动滑块填充拼图". Protocol (reverse-engineered from
//! that widget and the page's `anti-crawler-verify-html` bundle):
//!
//! 1. `GET getPreHandle?captcha_type=4&appid=souniu_fight_spider` (JSONP)
//!    → `{sign, urlParams, imgs: [bg_id, slice_id], initx, inity}`.
//! 2. `GET getImg?{urlParams}&iuk={id}` → 340×195 JPEG background with a
//!    jigsaw gap, plus a small RGBA PNG slice.
//! 3. Gap x-position via boundary-continuity matching (see [`find_gap`]).
//! 4. `GET getTicket?{urlParams}&phrase={x};{inity};{w};{h}` (JSONP)
//!    → `{ticket}`.
//! 5. `POST www.iwencai.com/ac_verification/check` form
//!    `{ticket, phrase, signature: sign, captcha_type: 4}`; response
//!    `code == 1003` means "wrong, refresh", `code == 0` is success.
//!
//! Widget geometry (from `dealOpt`/`handleDragMove`): display width 280 px
//! (set by the iwencai page), so `scale = 280/340`, `height = 280/340*195`,
//! and the drag distance equals the background-image gap offset times
//! `scale`. No mouse-track data is submitted by this widget version — only
//! the final phrase. Verified live 2026-08-22: display-scaled phrase
//! obtained a real ticket; image-coordinate phrases are rejected with
//! `{"code":-1,"msg":"Phrase Error."}`.
//!
//! ## Why not ddddocr
//!
//! `ddddocr-tract`'s `slide_match` (canny + template matching) located the
//! gap at x=102 on a live-captured sample where the true gap is x≈155
//! (confirmed visually and by a live `getTicket` accept) — cloud/sky
//! backgrounds defeat generic edge matching. The boundary-continuity metric
//! below scored x=157 on the same sample and x=176/163 on two further live
//! challenges, both accepted by `getTicket`.

use serde::Deserialize;
use serde_json::Value;

use crate::error::WencaiError;
use crate::wencai::WencaiClient;

const CAPTCHA_BASE: &str = "http://captcha.10jqka.com.cn";
const CHECK_URL: &str = "http://www.iwencai.com/ac_verification/check";

/// Display width the iwencai page initializes the widget with.
const WIDGET_WIDTH: f64 = 280.0;
/// Natural width of the background image (always 340 for captcha_type=4).
const IMAGE_WIDTH: f64 = 340.0;
/// Natural height of the background image.
const IMAGE_HEIGHT: f64 = 195.0;

/// `code` returned by `/ac_verification/check` when the answer is wrong.
const CHECK_CODE_RETRY: i64 = 1003;

/// Simulated human drag time before submitting, in milliseconds. Solving
/// and submitting within <1 s of the image fetch looks robotic; the real
/// widget's user needs at least a second or two.
const HUMAN_DRAG_DELAY_MS: u64 = 1500;

#[derive(Debug, Deserialize)]
struct PreHandleBody {
    data: PreHandleData,
    code: i64,
}

#[derive(Debug, Deserialize)]
struct PreHandleData {
    sign: String,
    #[serde(rename = "urlParams")]
    url_params: String,
    imgs: Vec<String>,
    inity: f64,
}

/// Solve one slider challenge end-to-end. On success the verification is
/// tied to the client's session (IP + `v` cookie) and the query may be
/// retried.
pub(crate) async fn solve_slider(client: &WencaiClient) -> Result<(), WencaiError> {
    // 1. PreHandle: sign + image ids.
    let random = rand_fraction() * now_millis();
    let pre_url = format!(
        "{CAPTCHA_BASE}/getPreHandle?captcha_type=4&appid=souniu_fight_spider&random={random}&callback=PreHandle"
    );
    let pre_text = client.get_text(&pre_url).await?;
    let pre: PreHandleBody = serde_json::from_str(jsonp_body(&pre_text, "PreHandle")?)
        .map_err(|e| WencaiError::Parse(format!("getPreHandle body: {e}")))?;
    if pre.code != 0 || pre.data.imgs.len() < 2 {
        return Err(WencaiError::Parse(format!(
            "getPreHandle code={} imgs={:?}",
            pre.code, pre.data.imgs
        )));
    }

    // 2. Fetch background + slice images.
    let bg = fetch_image(client, &pre.data.url_params, &pre.data.imgs[0]).await?;
    let slice = fetch_image(client, &pre.data.url_params, &pre.data.imgs[1]).await?;

    // 3. Locate the gap; convert to widget display coordinates.
    let gap = find_gap(&slice, &bg, pre.data.inity)?;
    let scale = WIDGET_WIDTH / IMAGE_WIDTH;
    let x = gap.drag_image_px * scale;
    let inity = pre.data.inity * scale;
    let phrase = format!(
        "{x};{inity};{};{}",
        WIDGET_WIDTH,
        WIDGET_WIDTH / IMAGE_WIDTH * IMAGE_HEIGHT
    );
    tracing::debug!(
        gap_x = gap.gap_x,
        score = gap.score,
        drag_image_px = gap.drag_image_px,
        x,
        %phrase,
        "slider gap solved"
    );

    // 4. Exchange the phrase for a ticket (after a human-ish drag pause).
    tokio::time::sleep(std::time::Duration::from_millis(HUMAN_DRAG_DELAY_MS)).await;
    let ticket_url = format!(
        "{CAPTCHA_BASE}/getTicket?{}&phrase={phrase}&callback=verify",
        pre.data.url_params
    );
    let ticket_text = client.get_text(&ticket_url).await?;
    let ticket: Value = serde_json::from_str(jsonp_body(&ticket_text, "verify")?)
        .map_err(|e| WencaiError::Parse(format!("getTicket body: {e}")))?;
    if ticket.get("code").and_then(Value::as_i64) != Some(0) {
        return Err(WencaiError::CaptchaFailed {
            attempts: 1,
            last_reason: format!(
                "getTicket rejected the phrase: {}",
                ticket_text.chars().take(200).collect::<String>()
            ),
        });
    }
    let ticket_str = ticket
        .get("ticket")
        .and_then(Value::as_str)
        .ok_or_else(|| WencaiError::Parse("getTicket response has no ticket".into()))?;

    // 5. Submit to iwencai.
    let form = [
        ("ticket".to_string(), ticket_str.to_string()),
        ("phrase".to_string(), phrase),
        ("signature".to_string(), pre.data.sign),
        ("captcha_type".to_string(), "4".to_string()),
    ];
    let check_text = client.post_form(CHECK_URL, &form).await?;
    let check: Value = serde_json::from_str(&check_text)
        .map_err(|e| WencaiError::Parse(format!("check body: {e}")))?;
    match check.get("code").and_then(Value::as_i64) {
        Some(CHECK_CODE_RETRY) => Err(WencaiError::CaptchaFailed {
            attempts: 1,
            last_reason: "check answered 1003 (wrong gap, refresh)".into(),
        }),
        Some(_) => Ok(()),
        None => Err(WencaiError::Parse(format!(
            "check response has no code: {}",
            check_text.chars().take(200).collect::<String>()
        ))),
    }
}

/// Result of gap detection.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Gap {
    /// Drag distance in background-image pixels (gap x minus the piece's
    /// own offset inside the slice image).
    pub drag_image_px: f64,
    /// Raw best-match x in the background image.
    pub gap_x: u32,
    /// Mean boundary color difference at the best position (lower = better).
    pub score: f64,
}

/// Locate the jigsaw gap by **boundary continuity**: at the true position,
/// the piece's edge pixels (original photo content) blend smoothly into the
/// background pixels just outside the gap (the gap interior is whited out,
/// but its surrounding is untouched). The score is the mean absolute RGB
/// difference across the boundary; the minimum wins.
///
/// The y search is constrained to ±3 px around the server-provided `inity`,
/// which eliminates most photographic false positives.
pub(crate) fn find_gap(
    slice_png: &[u8],
    bg_jpeg: &[u8],
    inity: f64,
) -> Result<Gap, WencaiError> {
    let slice = image::load_from_memory(slice_png)
        .map_err(|e| WencaiError::Parse(format!("slice decode: {e}")))?
        .to_rgba8();
    let bg = image::load_from_memory(bg_jpeg)
        .map_err(|e| WencaiError::Parse(format!("background decode: {e}")))?
        .to_rgb8();

    // Piece mask and its bounding box.
    let (sw, sh) = (slice.width(), slice.height());
    let mask: Vec<bool> = slice
        .pixels()
        .map(|p| p.0[3] > 100)
        .collect();
    let (mut x0, mut y0, mut x1, mut y1) = (sw, sh, 0, 0);
    for y in 0..sh {
        for x in 0..sw {
            if mask[(y * sw + x) as usize] {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x0 > x1 || y0 > y1 {
        return Err(WencaiError::Parse("slice has no opaque pixels".into()));
    }
    let (pw, ph) = (x1 - x0 + 1, y1 - y0 + 1);

    // Piece boundary ring + outward direction from the piece centroid.
    let (mut cx, mut cy, mut n) = (0f64, 0f64, 0f64);
    for y in y0..=y1 {
        for x in x0..=x1 {
            if mask[(y * sw + x) as usize] {
                cx += f64::from(x - x0);
                cy += f64::from(y - y0);
                n += 1.0;
            }
        }
    }
    let (cx, cy) = (cx / n, cy / n);
    // (px, py, dx, dy): boundary pixel (piece-bbox coords) + outward step.
    let mut ring: Vec<(u32, u32, i64, i64)> = Vec::new();
    let at = |x: u32, y: u32| mask[(y * sw + x) as usize];
    for y in y0..=y1 {
        for x in x0..=x1 {
            if !at(x, y) {
                continue;
            }
            let boundary = (x == 0 || !at(x - 1, y))
                || (x + 1 >= sw || !at(x + 1, y))
                || (y == 0 || !at(x, y - 1))
                || (y + 1 >= sh || !at(x, y + 1));
            if !boundary {
                continue;
            }
            let (rx, ry) = (x - x0, y - y0);
            let dx = (f64::from(rx) - cx).signum() as i64;
            let dy = (f64::from(ry) - cy).signum() as i64;
            // The outward neighbour must lie outside the piece.
            let (nx, ny) = (i64::from(x) + dx, i64::from(y) + dy);
            let outside = nx < 0
                || ny < 0
                || nx >= i64::from(sw)
                || ny >= i64::from(sh)
                || !mask[(ny as u32 * sw + nx as u32) as usize];
            if outside {
                ring.push((rx, ry, dx, dy));
            }
        }
    }
    if ring.len() < 30 {
        return Err(WencaiError::Parse(format!(
            "piece boundary too small ({} px)",
            ring.len()
        )));
    }

    let (bw, bh) = (bg.width(), bg.height());
    let inity_i = inity.round() as i64;
    let mut best: Option<Gap> = None;
    let y_lo = (inity_i - 3).max(0) as u32;
    let y_hi = ((inity_i + 3) as u32).min(bh.saturating_sub(ph));
    for yy in y_lo..=y_hi {
        for xx in 40..bw.saturating_sub(pw) {
            let mut acc = 0f64;
            let mut cnt = 0u32;
            for &(rx, ry, dx, dy) in &ring {
                let (bx, by) = (xx + rx, yy + ry);
                let (ox, oy) = (i64::from(bx) + dx, i64::from(by) + dy);
                if ox < 0 || oy < 0 || ox >= i64::from(bw) || oy >= i64::from(bh) {
                    continue;
                }
                let p = slice.get_pixel(x0 + rx, y0 + ry).0;
                let b = bg.get_pixel(ox as u32, oy as u32).0;
                acc += f64::from(p[0].abs_diff(b[0]))
                    + f64::from(p[1].abs_diff(b[1]))
                    + f64::from(p[2].abs_diff(b[2]));
                cnt += 1;
            }
            if cnt < 30 {
                continue;
            }
            let score = acc / f64::from(cnt);
            if best.is_none_or(|g| score < g.score) {
                best = Some(Gap {
                    drag_image_px: f64::from(xx) - f64::from(x0),
                    gap_x: xx,
                    score,
                });
            }
        }
    }
    best.ok_or_else(|| WencaiError::Parse("no valid gap candidate".into()))
}

async fn fetch_image(
    client: &WencaiClient,
    url_params: &str,
    iuk: &str,
) -> Result<Vec<u8>, WencaiError> {
    let url = format!("{CAPTCHA_BASE}/getImg?{url_params}&iuk={iuk}");
    client.get_bytes(&url).await
}

/// Strip the `name(...)` JSONP wrapper, returning the inner JSON text.
fn jsonp_body<'a>(text: &'a str, callback: &str) -> Result<&'a str, WencaiError> {
    let text = text.trim();
    text.strip_prefix(callback)
        .and_then(|rest| rest.strip_prefix('('))
        .and_then(|rest| rest.strip_suffix(')'))
        .ok_or_else(|| {
            WencaiError::Parse(format!(
                "expected JSONP {callback}(...), got: {}",
                text.chars().take(200).collect::<String>()
            ))
        })
}

/// `Math.random()` stand-in: [0, 1) fraction from system entropy.
fn rand_fraction() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    f64::from(nanos) / f64::from(u32::MAX)
}

/// `new Date().getTime()` stand-in.
fn now_millis() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonp_unwrap() {
        let text = r#"PreHandle({"data":{"sign":"abc"},"code":0,"msg":"ok"})"#;
        let inner = jsonp_body(text, "PreHandle").unwrap();
        let v: Value = serde_json::from_str(inner).unwrap();
        assert_eq!(v["code"], 0);
        assert!(jsonp_body(text, "verify").is_err());
    }

    /// Gap detection on a real captcha pair captured live 2026-08-22
    /// (fixtures committed under tests/fixtures). Ground truth: the same
    /// boundary-continuity algorithm scored x=157 on this pair, the gap is
    /// visible at x≈155, and ddddocr's canny matcher wrongly said x=102.
    #[test]
    fn find_gap_real_sample() {
        let bg = include_bytes!("../tests/fixtures/slider_bg_sample.jpg");
        let slice = include_bytes!("../tests/fixtures/slider_slice_sample.png");
        let gap = find_gap(slice, bg, 38.0).expect("find_gap");
        assert!(
            (145..=170).contains(&gap.gap_x),
            "gap at x={}, expected 145..=170 (score {})",
            gap.gap_x,
            gap.score
        );
    }
}

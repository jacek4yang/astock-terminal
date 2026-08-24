/** Final renderer-side defence. The MoonBit Provider applies the same rule. */
export function stripPrivateReasoning(raw: string): string {
  return raw
    .replace(/<think>[\s\S]*?<\/think>/gi, "")
    .replace(/<think>[\s\S]*$/gi, "")
    .replace(/<(?:t(?:h(?:i(?:n(?:k)?)?)?)?)?$/i, "");
}

/**
 * Keep citations, source versions and research detail visible, but never show
 * an accidentally echoed credential. Rendering is plain text, so this is a
 * privacy boundary rather than an HTML sanitizer.
 */
export function sanitizeAgentVisibleText(raw: string): string {
  return stripPrivateReasoning(raw)
    .replace(/Bearer\s+[A-Za-z0-9._~+/=-]+/gi, "Bearer [已隐藏敏感信息]")
    .replace(
      /((?:api[-_ ]?key|token|secret|password|authorization|cookie|credential)\s*[=:]\s*)[^\s,;，；]+/gi,
      "$1[已隐藏敏感信息]",
    );
}

///|
test "structured response discards MiniMax think tags" {
  let value = model_json_content(
    "<think>private reasoning with {braces} that must not reach the UI</think>\n```json\n{\"answer\":\"ok\"}\n```",
  )
  assert_eq(value.stringify(), "{\"answer\":\"ok\"}")
}

///|
test "plain response publishes only final visible text" {
  let visible = visible_model_text(
    "<think>private reasoning that must not reach the UI</think>\n研究结论：证据仍不完整，应继续人工复核。",
  )
  assert_eq(visible, "研究结论：证据仍不完整，应继续人工复核。")
}

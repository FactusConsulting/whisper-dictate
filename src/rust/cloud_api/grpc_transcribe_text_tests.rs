use super::append_final_segment;

#[test]
fn final_segments_keep_word_boundaries_and_attach_punctuation() {
    let mut text = String::new();
    append_final_segment(&mut text, "hello");
    append_final_segment(&mut text, "world");
    append_final_segment(&mut text, "!");
    assert_eq!(text, "hello world!");
}

#[test]
fn final_segments_preserve_cjk_boundaries_and_punctuation() {
    let mut text = String::new();
    append_final_segment(&mut text, "你好");
    append_final_segment(&mut text, "世界");
    append_final_segment(&mut text, "。 ");
    assert_eq!(text, "你好世界。");
}

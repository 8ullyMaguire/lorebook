use lorebook_scan::filename::{parse_file_info, FileInfo};

#[test]
fn test_parse_simple() {
    let fi = parse_file_info("/a/b/Author - Title.epub");
    assert_eq!(fi.basename, "Author - Title");
    assert_eq!(fi.extension, Some("epub".to_string()));
}

#[test]
fn test_parse_no_ext() {
    let fi = parse_file_info("/a/b/JustAFile");
    assert_eq!(fi.basename, "JustAFile");
    assert_eq!(fi.extension, None);
}

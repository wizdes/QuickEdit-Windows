use qe_perf::foreground_thread_links;

#[test]
fn background_caller_attaches_to_foreground_and_target_threads() {
    assert_eq!(
        foreground_thread_links(10, 20, 30),
        vec![(10, 20), (30, 20)]
    );
}

#[test]
fn foreground_attachment_skips_missing_and_duplicate_threads() {
    assert_eq!(foreground_thread_links(10, 20, 10), vec![(10, 20)]);
    assert_eq!(
        foreground_thread_links(20, 20, 20),
        Vec::<(u32, u32)>::new()
    );
    assert_eq!(foreground_thread_links(10, 0, 30), Vec::<(u32, u32)>::new());
}

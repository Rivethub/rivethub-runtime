use wire::gateway::memory::BrowseWindow;

#[test]
fn window_serde_names_match_the_spec() {
    let expected = [
        (BrowseWindow::Today, "\"today\""),
        (BrowseWindow::Yesterday, "\"yesterday\""),
        (BrowseWindow::ThisMorning, "\"this_morning\""),
        (BrowseWindow::ThisWeek, "\"this_week\""),
        (BrowseWindow::Last24h, "\"last_24h\""),
        (BrowseWindow::Last7d, "\"last_7d\""),
        (BrowseWindow::Last14d, "\"last_14d\""),
    ];
    for (window, json) in expected {
        assert_eq!(serde_json::to_string(&window).unwrap(), json);
        assert_eq!(serde_json::from_str::<BrowseWindow>(json).unwrap(), window);
    }
}

#[test]
fn window_parse_normalizes_case_and_whitespace() {
    assert_eq!(BrowseWindow::parse("TODAY").unwrap(), BrowseWindow::Today);
    assert_eq!(
        BrowseWindow::parse("  last_7d\t").unwrap(),
        BrowseWindow::Last7d
    );
}

#[test]
fn window_parse_reports_empty_and_unknown() {
    let empty = BrowseWindow::parse("   ").unwrap_err().to_string();
    assert_eq!(empty, "window is empty");
    let unknown = BrowseWindow::parse("Fortnight").unwrap_err().to_string();
    assert_eq!(
        unknown,
        "unknown window \"Fortnight\"; expected today, yesterday, this_morning, this_week, last_24h, last_7d, last_14d"
    );
}

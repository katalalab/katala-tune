use fleet_studio::search_input::SearchInput;

#[test]
fn utf16_selection_never_splits_surrogate_pairs_or_mutates_on_invalid_ranges() {
    let mut input = SearchInput::new("x👩‍💻e\u{301}").unwrap();
    assert!(input.select_utf16(1..6, false));
    assert_eq!(input.selected_text(), "👩‍💻");
    assert_eq!(input.selection_utf16(), (1..6, false));
    assert!(!input.select_utf16(2..3, false));
    assert!(!input.select_utf16(100..101, false));
    assert_eq!(input.selected_text(), "👩‍💻");
}

#[test]
fn ime_selection_is_relative_to_new_text_after_nonempty_prefix() {
    let mut input = SearchInput::new("prefix-OLD-suffix").unwrap();
    assert!(input.select_utf16(7..10, false));
    assert!(input.mark_text(None, "日本", Some(1..2)));
    assert_eq!(input.text(), "prefix-日本-suffix");
    assert_eq!(input.selected_text(), "本");
    assert_eq!(input.marked_utf16(), Some(7..9));
    assert!(input.commit_text(None, "日本語"));
    assert_eq!(input.text(), "prefix-日本語-suffix");
    assert_eq!(input.marked_utf16(), None);
    assert_eq!(input.selection_utf16(), (10..10, false));
}

#[test]
fn ime_cancel_restores_the_replaced_selection_and_empty_event_without_mark_is_noop() {
    let mut input = SearchInput::new("before 選択 after").unwrap();
    assert!(input.select_utf16(7..9, false));
    assert!(input.mark_text(None, "にほん", None));
    assert!(input.commit_text(None, ""));
    assert_eq!(input.text(), "before 選択 after");
    assert_eq!(input.selected_text(), "選択");
    assert!(!input.commit_text(None, ""));
    assert_eq!(input.selected_text(), "選択");
}

#[test]
fn composition_does_not_change_the_applied_filter_until_commit() {
    let mut input = SearchInput::new("old").unwrap();
    assert!(input.select_utf16(0..3, false));
    assert!(input.mark_text(None, "new", None));
    assert_eq!(input.text(), "new");
    assert_eq!(input.filter_text(), "old");
    assert!(input.cancel_composition());
    assert_eq!(input.filter_text(), "old");
    assert!(input.mark_text(None, "new", None));
    assert!(input.commit_text(None, "new"));
    assert_eq!(input.filter_text(), "new");
}

#[test]
fn rejected_composition_blocks_activation_but_rejected_commit_restores_editing() {
    let mut input = SearchInput::new("safe").unwrap();
    assert!(input.select_utf16(1..3, false));
    assert!(!input.mark_text(None, &"あ".repeat(100), None));
    assert_eq!(input.text(), "safe");
    assert!(input.is_composing());
    assert!(!input.commit_text(None, &"あ".repeat(100)));
    assert!(!input.is_composing());
    assert_eq!(input.text(), "safe");
    assert_eq!(input.selected_text(), "af");
    assert!(input.commit_text(None, "ok"));
    assert_eq!(input.text(), "soke");
}

#[test]
fn invalid_commit_range_ends_composition_and_restores_original() {
    let mut input = SearchInput::new("safe").unwrap();
    assert!(input.mark_text(None, "日", None));
    assert!(!input.commit_text(Some(100..101), "日"));
    assert!(!input.is_composing());
    assert_eq!(input.text(), "safe");
}

#[test]
fn cursor_and_delete_follow_extended_grapheme_boundaries() {
    let mut input = SearchInput::new("a👩‍💻e\u{301}").unwrap();
    input.move_end(false);
    assert!(input.backspace());
    assert_eq!(input.text(), "a👩‍💻");
    input.move_left(true);
    assert_eq!(input.selected_text(), "👩‍💻");
    assert!(input.delete());
    assert_eq!(input.text(), "a");
    assert_eq!(input.selection_utf16(), (1..1, false));
}

#[test]
fn keyboard_deletion_does_not_leave_a_combining_fragment() {
    for backwards in [true, false] {
        let mut input = SearchInput::new("a\u{301}b").unwrap();
        assert!(input.select_utf16(1..1, false));
        assert!(if backwards { input.backspace() } else { input.delete() });
        assert_eq!(input.text(), "b");
    }
}

#[test]
fn committed_input_is_bounded_and_rejected_atomically() {
    let mut input = SearchInput::new("one two").unwrap();
    assert!(input.select_utf16(4..7, false));
    assert!(input.commit_text(None, "日本語"));
    assert_eq!(input.text(), "one 日本語");
    let before = input.text().to_owned();
    assert!(!input.commit_text(None, &"👩".repeat(100)));
    assert_eq!(input.text(), before);
    assert!(!input.commit_text(None, "bad\0value"));
    assert_eq!(input.text(), before);
}

#[test]
fn composition_enter_and_escape_are_for_ime_not_navigation() {
    use fleet_studio::search_input::{KeyAction, KeyModifiers, search_key};
    for key in ["enter", "escape"] {
        assert_eq!(search_key(key, KeyModifiers::default(), true, false), KeyAction::Ime);
    }
    assert_eq!(search_key("enter", KeyModifiers::default(), false, false), KeyAction::Confirm);
    assert_eq!(search_key("escape", KeyModifiers::default(), false, false), KeyAction::Leave);
}

#[test]
fn paste_and_select_all_use_the_platform_primary_modifier() {
    use fleet_studio::search_input::{KeyAction, KeyModifiers, search_key};
    let ctrl = KeyModifiers { control: true, ..Default::default() };
    let cmd = KeyModifiers { platform: true, ..Default::default() };
    assert_eq!(search_key("v", ctrl, false, false), KeyAction::Paste);
    assert_eq!(search_key("a", ctrl, false, false), KeyAction::SelectAll);
    assert_eq!(search_key("v", cmd, false, true), KeyAction::Paste);
    assert_eq!(search_key("a", cmd, false, true), KeyAction::SelectAll);
    assert_ne!(search_key("v", ctrl, false, true), KeyAction::Paste);
    assert_ne!(search_key("v", KeyModifiers { alt: true, ..ctrl }, false, false), KeyAction::Paste);
}

#[test]
fn ligature_caret_stops_and_hit_testing_match_character_boundaries() {
    use fleet_studio::search_input::{caret_stops, nearest_caret};
    let stops = caret_stops("ffi", |byte| if byte == 0 { 0.0 } else { 12.0 }).unwrap();
    assert_eq!(stops, vec![(0, 0.0), (1, 4.0), (2, 8.0), (3, 12.0)]);
    assert_eq!(nearest_caret(&stops, 3.5), Some(1));
    assert_eq!(nearest_caret(&stops, 8.5), Some(2));
    assert_eq!(nearest_caret(&stops, 100.0), Some(3));
    let emoji = caret_stops("a👩‍💻", |byte| {
        if byte == 0 {
            0.0
        } else if byte == 1 {
            5.0
        } else {
            20.0
        }
    })
    .unwrap();
    assert_eq!(nearest_caret(&emoji, 12.0), Some(1));
    assert_eq!(nearest_caret(&emoji, 19.0), Some("a👩‍💻".len()));
    assert!(caret_stops("ab", |byte| if byte == 1 { f32::NAN } else { byte as f32 }).is_none());
}

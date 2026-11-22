#[cfg(not(target_os = "linux"))]
compile_error!("GLib VariantStrIter regression tests require native Linux");

#[cfg(target_os = "linux")]
mod linux {
    use glib::{Variant, VariantStrIter, prelude::*};

    fn assert_remaining(iter: &VariantStrIter<'_>, remaining: usize) {
        assert_eq!(iter.len(), remaining);
        assert_eq!(iter.size_hint(), (remaining, Some(remaining)));
    }

    fn assert_exhausted(iter: &mut VariantStrIter<'_>) {
        for _ in 0..2 {
            assert_eq!(iter.next(), None);
            assert_eq!(iter.next_back(), None);
            assert_remaining(iter, 0);
        }
    }

    fn assert_directions(values: &[&str]) {
        let variant = Variant::array_from_iter::<String>(values.iter().map(|s| s.to_variant()));
        assert_eq!(variant.type_().as_str(), "as");
        assert_eq!(variant.n_children(), values.len());

        let mut forward = variant.array_iter_str().unwrap();
        assert_remaining(&forward, values.len());
        for (index, expected) in values.iter().enumerate() {
            assert_eq!(forward.next(), Some(*expected));
            assert_remaining(&forward, values.len() - index - 1);
        }
        assert_exhausted(&mut forward);

        let mut reverse = variant.array_iter_str().unwrap();
        assert_remaining(&reverse, values.len());
        for (index, expected) in values.iter().rev().enumerate() {
            assert_eq!(reverse.next_back(), Some(*expected));
            assert_remaining(&reverse, values.len() - index - 1);
        }
        assert_exhausted(&mut reverse);

        let mut mixed = variant.array_iter_str().unwrap();
        let (mut left, mut right) = (0, values.len());
        assert_remaining(&mixed, right - left);
        while left < right {
            assert_eq!(mixed.next(), Some(values[left]));
            left += 1;
            assert_remaining(&mixed, right - left);
            if left < right {
                right -= 1;
                assert_eq!(mixed.next_back(), Some(values[right]));
                assert_remaining(&mixed, right - left);
            }
        }
        assert_exhausted(&mut mixed);
    }

    #[test]
    fn empty() {
        assert_directions(&[]);
    }

    #[test]
    fn ordinary() {
        assert_directions(&["alpha", "", "omega", "tail"]);
    }

    #[test]
    fn unicode() {
        assert_directions(&["日本語", "naïve", "🦀", "e\u{301}", "終"]);
    }
}

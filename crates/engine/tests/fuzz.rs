//! Any query text, however malformed, must parse and search without a panic; and names survive Windows' UTF-16 and
//! back exactly, unpaired surrogates included.
use engine::search::{Search, Sort};
use engine::store::{Index, Meta, DIR, NONE};
use engine::walk::{from_wide, to_wide};
use proptest::prelude::*;

fn sample() -> Index {
    let mut index = Index::new();
    let root = index.push(NONE, "C:", Meta { size: None, modified: 0, flags: DIR });
    let folder = index.push(root, "Folder ñ", Meta { size: None, modified: 0, flags: DIR });
    for (n, name) in ["a.txt", "Ünï.TXT", "🦀", "x\u{10F800}y", "regex(.*)", r"\weird\\", "[*?]"].iter().enumerate() {
        index.push(if n % 2 == 0 { root } else { folder }, name, Meta { size: Some(n as u64), modified: 0, flags: 0 });
    }
    index.build_table();
    index.sort_names();
    index.rank_paths();
    index
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 20000, ..ProptestConfig::default() })]
    #[test]
    fn any_query(q in r#"\PC{0,40}|[!|*?\\:"a-z ]{0,20}|regex:\PC{0,20}|ext:[a-z;]{0,10}|[A-Z]:\\[a-z\\ ]{0,15}"#) {
        let index = sample();
        let search = Search::new(&q);
        for (sort, descending) in [(Sort::None, false), (Sort::Name, true), (Sort::Path, false), (Sort::Size, true), (Sort::Modified, false)] {
            let found = search.page(&index, None, sort, descending, 1, 3);
            prop_assert!(found.count <= index.live());
            prop_assert!(found.page.len() <= 3);
        }
    }

    #[test]
    fn names_round_trip(units in prop::collection::vec(any::<u16>(), 0..40)) {
        let name = from_wide(&units);
        prop_assert_eq!(to_wide(&name), units);
    }
}

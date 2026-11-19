// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

use super::*;

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn expressions_become_alternatives() {
    assert_eq!(
        alternatives("MIT OR Apache-2.0").unwrap(),
        vec![set(&["MIT"]), set(&["Apache-2.0"])]
    );
    assert_eq!(
        alternatives("MIT/Apache-2.0").unwrap(),
        alternatives("MIT OR Apache-2.0").unwrap()
    );
    assert_eq!(
        alternatives("(MIT OR Apache-2.0) AND Unicode-3.0").unwrap(),
        vec![
            set(&["MIT", "Unicode-3.0"]),
            set(&["Apache-2.0", "Unicode-3.0"])
        ]
    );
    assert_eq!(
        alternatives("Apache-2.0 WITH LLVM-exception OR MIT").unwrap(),
        vec![set(&["Apache-2.0", "LLVM-exception"]), set(&["MIT"])]
    );
    assert_eq!(
        alternatives("GPL-3.0-or-later").unwrap(),
        vec![set(&["GPL-3.0"])]
    );
    assert!(alternatives("MIT OR").is_err());
    assert!(alternatives("(MIT").is_err());
}

#[test]
fn standard_texts_are_recognised() {
    for (id, text) in STANDARD_TEXTS {
        let ids = classify(text);
        assert!(ids.contains(id), "{id}: classified as {ids:?}");
    }
}

#[test]
fn similar_licences_are_told_apart() {
    let isc = "Permission to use, copy, modify, and/or distribute this software for any\n\
               purpose with or without fee is hereby granted, provided that the above\n\
               copyright notice and this permission notice appear in all copies.";
    assert_eq!(classify(isc), BTreeSet::from(["ISC"]));
    let zero_bsd = "Permission to use, copy, modify, and/or distribute this software for any\n\
                    purpose with or without fee is hereby granted.";
    assert_eq!(classify(zero_bsd), BTreeSet::from(["0BSD"]));
    let x11 = "Permission is hereby granted, free of charge, to any person obtaining a\n\
               copy of this software ... The above copyright notice and this permission\n\
               notice (including the next paragraph) shall be included in all copies.";
    assert_eq!(classify(x11), BTreeSet::from(["MIT"]));
    let commented = "// Permission to use, copy, modify, and/or distribute this software for any\n\
                     // purpose with or without fee is hereby granted, provided that the above\n\
                     // copyright notice and this permission notice appear in all copies.";
    assert_eq!(classify(commented), BTreeSet::from(["ISC"]));
    assert!(classify("Licensed under either of Apache-2.0 or MIT, at your option.").is_empty());
}

#[test]
fn copyright_lines() {
    for notice in [
        "Copyright (c) 2016 Alex Crichton",
        "  Copyright 2022 - 2025 Zed Industries, Inc.",
        "Copyright © 2020 Someone",
        "COPYRIGHT (C) 2001 Someone",
        "(c) 2019 Someone",
        "// Copyright 2015-2016 Brian Smith.",
        "Copyright The Rust Project Developers",
    ] {
        assert_eq!(copyright_line(notice), Line::Notice, "{notice}");
    }
    for placeholder in [
        "Copyright [yyyy] [name of copyright owner]",
        "Copyright (c) <year> <copyright holders>",
        "   Copyright {yyyy} {name of copyright owner}",
    ] {
        assert_eq!(
            copyright_line(placeholder),
            Line::Placeholder,
            "{placeholder}"
        );
    }
    for text in [
        "COPYRIGHT HOLDERS AND CONTRIBUTORS \"AS IS\" AND ANY EXPRESS OR IMPLIED",
        "copyright notice and this permission notice shall be included",
        "copyright on the Program, and are irrevocable provided the stated",
        "COPYRIGHT AND PERMISSION NOTICE",
        "Copyright (C) 2007 Free Software Foundation, Inc. <https://fsf.org/>",
        "Copyright and related rights",
        "copyrighted work",
    ] {
        assert_eq!(copyright_line(text), Line::Text, "{text}");
    }
}

#[test]
fn copyright_is_split_from_the_licence() {
    let text = "Copyright 2001-2023 Xiph.Org, Skype Limited,\n\
                \x20                   Jean-Marc Valin\n\
                Copyright (c) 2010 Someone\n\
                All rights reserved.\n\
                \n\
                Redistribution and use in source and binary forms ...";
    let (notices, body) = split_copyright(text);
    assert_eq!(
        notices,
        [
            "Copyright 2001-2023 Xiph.Org, Skype Limited, Jean-Marc Valin",
            "Copyright (c) 2010 Someone All rights reserved.",
        ]
    );
    assert_eq!(
        body,
        "Redistribution and use in source and binary forms ..."
    );
    // Copies that differ only in their copyright lines group together.
    let (_, other) = split_copyright(
        "Copyright 2015 Other\n\nRedistribution and use in source and binary forms ...",
    );
    assert_eq!(normalized(&body), normalized(&other));
}

#[test]
fn apache_appendix_is_trimmed() {
    let ids = BTreeSet::from(["Apache-2.0"]);
    let terms = "TERMS AND CONDITIONS ...\n   END OF TERMS AND CONDITIONS";
    let with_appendix = format!(
        "{terms}\n\n   APPENDIX: How to apply the Apache License to your work.\n\n   \
         Licensed under the Apache License, Version 2.0 (the \"License\");\n   ...\n   \
         limitations under the License."
    );
    assert_eq!(trim_apache_appendix(&ids, with_appendix), terms);
    let extra = format!("{terms}\n\nThis crate also includes code under the MIT licence.");
    assert_eq!(trim_apache_appendix(&ids, extra.clone()), extra);
    let both = BTreeSet::from(["Apache-2.0", "MIT"]);
    assert_eq!(
        trim_apache_appendix(&both, format!("{terms}\n\nMIT ...")),
        format!("{terms}\n\nMIT ...")
    );
}

#[test]
fn licence_file_names() {
    for name in [
        "LICENSE",
        "LICENSE-MIT",
        "license-apache-2.0",
        "LICENCE",
        "COPYING",
        "COPYRIGHT",
        "NOTICE",
        "UNLICENSE",
        "LICENSE.md",
        "LICENSE-APACHE.txt",
    ] {
        assert!(is_licence_file(name), "{name}");
    }
    for name in [
        "README.md",
        "Cargo.toml",
        "license.rs",
        "copyright.svg",
        "src",
    ] {
        assert!(!is_licence_file(name), "{name}");
    }
}

#[test]
fn text_is_cleaned() {
    assert_eq!(
        clean_text(b"\xef\xbb\xbf\r\n  MIT  \r\n\r\n\r\n\r\nText\x0c\r\n\r\n"),
        "  MIT\n\nText"
    );
}

#[test]
fn platforms_are_named() {
    assert_eq!(platforms(&BTreeSet::from([0, 1])), "Linux");
    assert_eq!(platforms(&BTreeSet::from([2, 3])), "Windows");
    assert_eq!(
        platforms(&BTreeSet::from([0, 2])),
        "Linux x86-64, Windows x86-64"
    );
    assert_eq!(
        platforms(&BTreeSet::from([0, 1, 3])),
        "Linux, Windows ARM64"
    );
}

#[test]
fn long_lines_wrap() {
    let mut s = String::new();
    push_wrapped(
        &mut s,
        "  Texts:     ",
        &["T1 (LICENSE-APACHE)"; 6].join(", "),
    );
    for line in s.lines() {
        assert!(line.chars().count() <= 78, "{line}");
    }
    assert!(s.lines().nth(1).unwrap().starts_with("             T1"));
}

#[test]
fn the_section_is_spliced() {
    let file = "Head\n\n[BEGIN grammar notices: x]\ng\n[END grammar notices]\n";
    let added = splice(file, "one\n").unwrap();
    assert_eq!(
        added,
        format!("{}\n\n{BEGIN}\n\none\n{END}\n", file.trim_end())
    );
    let replaced = splice(&added, "two\n").unwrap();
    assert_eq!(replaced, added.replace("one", "two"));
    assert!(splice(&format!("{BEGIN}\n"), "x").is_err());
}

#[test]
fn deny_policy_is_read() {
    let policy = Policy::load(&crate::workspace_root()).unwrap();
    assert!(policy.allows("anything", "MIT"));
    assert!(policy.allows("anything", "GPL-3.0"));
    assert!(policy.allows("anything", "LLVM-exception"));
    assert!(!policy.allows("anything", "GPL-2.0"));
    assert!(!policy.allows("anything", "EUPL-1.2"));
    assert!(policy.allows("giallo", "EUPL-1.2"));
}

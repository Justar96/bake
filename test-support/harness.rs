//! Libtest discovery and selection for binaries that also serve as child fixtures.

#[derive(Default)]
pub struct HarnessArgs {
    list: bool,
    exact: bool,
    only_ignored: bool,
    filters: Vec<String>,
    skips: Vec<String>,
}

impl HarnessArgs {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Self {
        let mut parsed = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--list" => parsed.list = true,
                "--exact" => parsed.exact = true,
                "--ignored" => parsed.only_ignored = true,
                "--skip" => parsed.skips.extend(args.next()),
                "--format" | "--color" | "--test-threads" | "--logfile" | "--shuffle-seed"
                | "-Z" => {
                    args.next();
                }
                flag if flag.starts_with('-') => {}
                filter => parsed.filters.push(filter.to_owned()),
            }
        }
        parsed
    }

    pub fn matches(&self, name: &str) -> bool {
        // These harnesses have no ignored cases.
        !self.only_ignored
            && !self.skips.iter().any(|skip| name.contains(skip))
            && (self.filters.is_empty()
                || self.filters.iter().any(|filter| {
                    if self.exact {
                        name == filter
                    } else {
                        name.contains(filter)
                    }
                }))
    }

    /// Print only discovery records, without starting any case or fixture.
    pub fn list<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> bool {
        if !self.list {
            return false;
        }
        for name in names {
            if self.matches(name) {
                println!("{name}: test");
            }
        }
        true
    }
}

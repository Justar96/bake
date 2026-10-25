//! The account a killed Windows tree is settled on: every process its job
//! reported as added, each held by handle until it is signalled, checked
//! against the job's lifetime count of associated processes.
//!
//! Why: a job's `ActiveProcesses` reaches 0 before a terminated member's
//! process handle is signalled (observed natively in every probe run), and
//! a member then no longer appears in the job's process-id list. Only a
//! signalled process handle is documented proof that a process has ended
//! (`TerminateProcess`), so each member must be named and awaited.
//!
//! Members are named by the job's `JOB_OBJECT_MSG_NEW_PROCESS` messages on
//! a completion port associated with the job while it is still empty, so
//! every association is after it. Delivery is documented as not
//! guaranteed, and nothing documents one message per association, so a
//! message is never trusted to be complete or unique:
//!
//! - **Counted** ([`Ledger::counted`]) are distinct members, never more
//!   than the associations that produced them: an in-job process
//!   identity (pid with creation time) counts once however many messages
//!   name it, and a pid that could not be opened or names a process outside
//!   the job counts only the first time that pid is seen. Anything
//!   ambiguous counts nothing, so the count can fall short, never over.
//! - **Settled** ([`Ledger::verdict`]) only when that count equals the
//!   job's `TotalProcesses` both before and after every counted member was
//!   observed ended, with no member unresolved and no active process. Fewer
//!   counted than associated (a lost message, a pid reused inside the job
//!   before it was opened that no exit message tells apart, a failed
//!   association counted by the job) never settles; more is a contradiction
//!   that never settles either.
//!
//! A pid reused inside the job is also counted from the job's exit
//! messages (`JOB_OBJECT_MSG_EXIT_PROCESS` and
//! `JOB_OBJECT_MSG_ABNORMAL_EXIT_PROCESS`, [`Ledger::exited`]). A completion
//! port delivers one job's messages in the order they were queued; a
//! process's exit is queued after its own new-process message, and before
//! its pid can be released and given to the next process. So a pid's first
//! new-process message, and each later one that follows an exit message for
//! the pid, name distinct processes: a second lower bound on the pid's
//! associations beside the distinct members opened. Each pid counts the
//! larger of the two, and only once a member has been resolved for it.
//! Without the second, a churning tree whose short children reuse an ended
//! member's pid and release it before the account opens it leaves the count
//! short for good (observed natively in `tests/windows_job_rundown.rs`'s
//! idle churn). A missing exit message leaves the count short, as before;
//! only a new-process message queued again after its own process's exit
//! could count one association twice, which makes the account a
//! contradiction unless a message is also lost.
//!
//! [`Opened::NoSuchProcess`] and [`Opened::OtherProcess`] mean the reported
//! member released its pid. Microsoft's [process-id lifetime explanation]
//! ties the pid to the process object, which retains itself while running;
//! even an exited process remains openable while its object exists. This
//! supports inferring completed exit from a missing or reused pid. Other
//! open/query failures remain unknown. The native probe also saw the pid
//! stay openable, in the job, throughout the unsignalled rundown window.
//!
//! [process-id lifetime explanation]: https://devblogs.microsoft.com/oldnewthing/20110107-00/?p=11803/
//!
//! Pure bookkeeping over a [`Host`], so the rules are tested on every
//! platform; the Windows host is in `windows_job.rs`.

use std::collections::{HashMap, HashSet};

/// The most distinct pids one account keeps. A pid is one record however
/// often it is reused, so this bounds the account by the system's pid
/// space, not by how many processes a long-lived tree starts; Windows pids
/// are multiples of 4 and reused, so a real tree stays far below it. One
/// more distinct pid is not dropped: it makes the account a contradiction,
/// which never settles.
pub(crate) const MAX_RECORDS: usize = 1 << 20;

/// What opening a reported pid found.
#[derive(Debug)]
pub(crate) enum Opened<H> {
    /// No process has this pid (`ERROR_INVALID_PARAMETER`). Read as the
    /// reported process having ended (see the module docs).
    NoSuchProcess,
    /// The pid names a process outside the job: the reported one released
    /// it, so it ended (see the module docs).
    OtherProcess,
    /// A process in the job, held by `handle`; a held handle keeps its pid
    /// from being reused. `created` is its creation time, `None` when that
    /// query failed (a process in rundown, or one whose security refuses
    /// it, may refuse a query its handle was opened for): it is still a
    /// member to await by handle, never one that ended, but a later process
    /// with its pid cannot be told apart from it.
    Member { created: Option<u64>, handle: H },
    /// The pid could not be opened for a reason other than no such process
    /// (access denied), or its membership could not be read: unknown, never
    /// ended.
    Unknown,
}

/// The operating system, as the ledger needs it.
pub(crate) trait Host {
    type Handle;
    fn open(&mut self, pid: u32) -> Opened<Self::Handle>;
    /// Whether the handle is signalled; `None` when that cannot be read.
    fn signalled(&mut self, handle: &Self::Handle) -> Option<bool>;
}

#[derive(Debug)]
enum Record<H> {
    /// A counted member not yet seen signalled.
    Live { created: Option<u64>, handle: H },
    /// A counted member seen signalled; its handle is closed, so its pid
    /// may be reused, which a known creation time tells apart.
    Ended { created: Option<u64> },
    /// A counted pid that could not be opened or named another process.
    Vanished,
    /// Reported, but unknown so far: counts nothing until resolved.
    Unresolved,
}

/// Whether a job's account is settled: one settle poll's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every associated process was named and observed ended.
    Settled,
    /// Not yet: waiting on members, messages, or a stable count.
    Pending(&'static str),
    /// The account cannot be trusted again; never settles.
    Contradiction(&'static str),
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Settled => f.write_str("settled"),
            Verdict::Pending(why) => write!(f, "not settled: {why}"),
            Verdict::Contradiction(why) => write!(f, "the account contradicts itself: {why}"),
        }
    }
}

/// One pid's lower bounds on the processes the job associated with it.
#[derive(Debug, Default)]
struct Tally {
    /// Distinct members opened with the pid, or inferred from it.
    members: u64,
    /// Its first new-process message and each later one that followed an
    /// exit message for it (see the module docs).
    messages: u64,
}

impl Tally {
    /// Counts nothing until a member has been resolved for the pid, so an
    /// unknown report stays ambiguous.
    fn counted(&self) -> u64 {
        if self.members == 0 {
            0
        } else {
            self.members.max(self.messages)
        }
    }
}

/// The account of one job.
#[derive(Debug)]
pub(crate) struct Ledger<H> {
    /// One record per reported pid: bounded by distinct pids, not by how
    /// many processes the job ever had.
    records: HashMap<u32, Record<H>>,
    /// The pids whose record is `Live` or `Unresolved`: what a check or a
    /// retry visits, so neither walks the ended records of a long life.
    open: HashSet<u32>,
    /// Recorded pids with an exit message since their last new-process
    /// message; bounded by the records.
    exited: HashSet<u32>,
    /// Each recorded pid's two lower bounds; `counted` is their sum.
    tallies: HashMap<u32, Tally>,
    counted: u64,
    contradiction: Option<&'static str>,
}

impl<H> Default for Ledger<H> {
    fn default() -> Self {
        Self {
            records: HashMap::new(),
            open: HashSet::new(),
            exited: HashSet::new(),
            tallies: HashMap::new(),
            counted: 0,
            contradiction: None,
        }
    }
}

impl<H> Ledger<H> {
    /// Distinct members counted so far.
    #[cfg_attr(not(test), allow(dead_code, reason = "read by the tests"))]
    pub(crate) fn counted(&self) -> u64 {
        self.counted
    }

    /// How many members are held by handle or unresolved.
    #[cfg_attr(not(test), allow(dead_code, reason = "read by the tests"))]
    pub(crate) fn open_members(&self) -> usize {
        self.open.len()
    }

    /// Marks the account untrustworthy for good.
    pub(crate) fn contradict(&mut self, why: &'static str) {
        self.contradiction.get_or_insert(why);
    }

    /// Records one exit message naming `pid`. A pid the account has not
    /// recorded is ignored: its new-process message was lost, which keeps
    /// the count short anyway.
    pub(crate) fn exited(&mut self, pid: u32) {
        if self.records.contains_key(&pid) {
            self.exited.insert(pid);
        }
    }

    /// Records one `JOB_OBJECT_MSG_NEW_PROCESS` naming `pid`.
    pub(crate) fn reported(&mut self, pid: u32, host: &mut impl Host<Handle = H>) {
        if pid == 0 {
            // No process is created with pid 0: not a message to trust.
            self.contradict("a new-process message named pid 0");
            return;
        }
        if !self.records.contains_key(&pid) && self.records.len() >= MAX_RECORDS {
            self.contradict("more distinct pids than the account keeps");
            return;
        }
        // Taken by every new-process message, so it marks only an exit
        // between this message and the one before it.
        let after_exit = self.exited.remove(&pid);
        let first = !self.records.contains_key(&pid);
        let members = self.observe(pid, host);
        self.tally(pid, members, u64::from(first || after_exit));
    }

    /// Adds to `pid`'s tally, and the change in its count to the total.
    fn tally(&mut self, pid: u32, members: u64, messages: u64) {
        let tally = self.tallies.entry(pid).or_default();
        let before = tally.counted();
        tally.members += members;
        tally.messages += messages;
        // Both bounds only grow, so the count never falls.
        self.counted += tally.counted() - before;
    }

    /// Opens `pid` and records what it names; returns how many members not
    /// counted before that names.
    fn observe(&mut self, pid: u32, host: &mut impl Host<Handle = H>) -> u64 {
        let mut members = 0;
        let opened = host.open(pid);
        let record = self.records.remove(&pid);
        let next = match (record, opened) {
            (None | Some(Record::Unresolved), Opened::NoSuchProcess | Opened::OtherProcess) => {
                members += 1;
                Record::Vanished
            }
            (None | Some(Record::Unresolved), Opened::Member { created, handle }) => {
                members += 1;
                Record::Live { created, handle }
            }
            (None | Some(Record::Unresolved), Opened::Unknown) => Record::Unresolved,
            // A held handle pins the pid: it can only name the same member.
            (Some(Record::Live { created, handle }), Opened::Member { created: now, .. }) => {
                if let (Some(created), Some(now)) = (created, now)
                    && created != now
                {
                    self.contradict("a held member's pid named another member");
                }
                Record::Live {
                    created: created.or(now),
                    handle,
                }
            }
            (Some(live @ Record::Live { .. }), Opened::NoSuchProcess | Opened::OtherProcess) => {
                self.contradict("a held member's pid was released");
                live
            }
            (Some(live @ Record::Live { .. }), Opened::Unknown) => live,
            (
                Some(Record::Ended { created }),
                Opened::Member {
                    created: now,
                    handle,
                },
            ) => match (created, now) {
                // A different member reusing the pid.
                (Some(ended), Some(now)) if ended != now => {
                    members += 1;
                    Record::Live {
                        created: Some(now),
                        handle,
                    }
                }
                // The same, ended member named again, or one that cannot
                // be told from it: counted nothing.
                _ => Record::Ended { created },
            },
            (Some(Record::Vanished), Opened::Member { created, handle }) => {
                // The vanished one released the pid before this member
                // existed (see the module docs), so this is another one.
                members += 1;
                Record::Live { created, handle }
            }
            // A pid seen before that cannot be told apart from an earlier
            // report: counted nothing, so the account can only fall short.
            (Some(seen @ (Record::Ended { .. } | Record::Vanished)), _) => seen,
        };
        if matches!(next, Record::Live { .. } | Record::Unresolved) {
            self.open.insert(pid);
        } else {
            self.open.remove(&pid);
        }
        self.records.insert(pid, next);
        members
    }

    /// Opens again every reported pid still unknown.
    pub(crate) fn retry(&mut self, host: &mut impl Host<Handle = H>) {
        let unresolved: Vec<u32> = self
            .open
            .iter()
            .filter(|pid| matches!(self.records.get(pid), Some(Record::Unresolved)))
            .copied()
            .collect();
        for pid in unresolved {
            let members = self.observe(pid, host);
            self.tally(pid, members, 0);
        }
    }

    /// Checks every live member's handle once, closing those signalled.
    /// Returns how many are still not signalled, or unresolved; `Err` when a
    /// handle cannot be checked.
    pub(crate) fn check(&mut self, host: &mut impl Host<Handle = H>) -> Result<usize, ()> {
        let mut waiting = 0;
        let mut failed = false;
        let mut ended = Vec::new();
        for pid in &self.open {
            match self.records.get_mut(pid) {
                Some(Record::Live { created, handle }) => match host.signalled(handle) {
                    Some(true) => ended.push((*pid, *created)),
                    Some(false) => waiting += 1,
                    None => failed = true,
                },
                Some(Record::Unresolved) => waiting += 1,
                // The index names only open records.
                _ => failed = true,
            }
        }
        for (pid, created) in ended {
            // Replacing the record closes the handle.
            self.records.insert(pid, Record::Ended { created });
            self.open.remove(&pid);
        }
        if failed { Err(()) } else { Ok(waiting) }
    }

    /// Whether the job's tree has ended. `before` is `TotalProcesses` read
    /// before the members were checked, `after` and `active` are read after;
    /// `waiting` is [`Self::check`]'s result in between. Settled only when
    /// nothing waits, the counted members equal both totals (so every
    /// association up to `before` was named and has ended, and none was made
    /// up to `after`, by which time no member was left to make one), and no
    /// process is active.
    pub(crate) fn verdict(
        &self,
        before: u32,
        after: u32,
        active: u32,
        waiting: Result<usize, ()>,
    ) -> Verdict {
        if let Some(why) = self.contradiction {
            return Verdict::Contradiction(why);
        }
        if after < before {
            return Verdict::Contradiction("the job's lifetime count went down");
        }
        if self.counted > u64::from(before) && before == after {
            return Verdict::Contradiction("more members counted than the job associated");
        }
        match waiting {
            Err(()) => Verdict::Pending("a member's handle could not be checked"),
            Ok(0) if self.counted < u64::from(before) => {
                Verdict::Pending("fewer members named than the job associated")
            }
            Ok(0) if before != after || self.counted != u64::from(after) => {
                Verdict::Pending("the job associated more processes meanwhile")
            }
            Ok(0) if active != 0 => Verdict::Pending("the job still counts an active process"),
            Ok(0) => Verdict::Settled,
            Ok(_) => Verdict::Pending("a named member has not ended"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted process table: each pid's current state, and whether its
    /// handles are signalled.
    #[derive(Default)]
    struct Table {
        /// pid -> (in job, creation time, signalled); absent is no process.
        processes: HashMap<u32, (bool, u64, bool)>,
        /// pids whose open fails as unknown.
        denied: Vec<u32>,
        /// Creation times whose signalled state cannot be read.
        unreadable: Vec<u64>,
        /// Processes (by creation time) whose creation time query fails.
        untimed: Vec<u64>,
    }

    impl Host for Table {
        /// A handle is the creation time it was opened at, so it stays the
        /// same process even if the table's pid is later reused.
        type Handle = u64;
        fn open(&mut self, pid: u32) -> Opened<u64> {
            if self.denied.contains(&pid) {
                return Opened::Unknown;
            }
            match self.processes.get(&pid) {
                None => Opened::NoSuchProcess,
                Some((false, ..)) => Opened::OtherProcess,
                Some((true, created, _)) => Opened::Member {
                    created: (!self.untimed.contains(created)).then_some(*created),
                    handle: *created,
                },
            }
        }
        fn signalled(&mut self, handle: &u64) -> Option<bool> {
            if self.unreadable.contains(handle) {
                return None;
            }
            Some(
                self.processes
                    .values()
                    .find(|(_, created, _)| created == handle)
                    .is_none_or(|(_, _, signalled)| *signalled),
            )
        }
    }

    fn settle(ledger: &mut Ledger<u64>, table: &mut Table, total: u32, active: u32) -> Verdict {
        ledger.retry(table);
        let waiting = ledger.check(table);
        ledger.verdict(total, total, active, waiting)
    }

    #[test]
    fn every_reported_member_ended_settles() {
        let mut table = Table::default();
        table.processes.insert(4, (true, 1, false));
        table.processes.insert(8, (true, 2, false));
        let mut ledger = Ledger::default();
        ledger.reported(4, &mut table);
        ledger.reported(8, &mut table);
        // The job counts no active process, but neither handle is
        // signalled: the early zero the probe observed.
        assert_eq!(
            settle(&mut ledger, &mut table, 2, 0),
            Verdict::Pending("a named member has not ended")
        );
        table.processes.get_mut(&4).expect("4").2 = true;
        table.processes.get_mut(&8).expect("8").2 = true;
        assert_eq!(settle(&mut ledger, &mut table, 2, 0), Verdict::Settled);
    }

    // Failure control: a lost new-process message leaves the account short,
    // and it stays short whatever else ends.
    #[test]
    fn a_dropped_message_never_settles() {
        let mut table = Table::default();
        table.processes.insert(4, (true, 1, true));
        // Pid 8 was associated, and is still running, but never reported.
        table.processes.insert(8, (true, 2, false));
        let mut ledger = Ledger::default();
        ledger.reported(4, &mut table);
        for _ in 0..3 {
            assert_eq!(
                settle(&mut ledger, &mut table, 2, 0),
                Verdict::Pending("fewer members named than the job associated")
            );
        }
        table.processes.get_mut(&8).expect("8").2 = true;
        assert_eq!(
            settle(&mut ledger, &mut table, 2, 0),
            Verdict::Pending("fewer members named than the job associated")
        );
    }

    // Failure control: a member that cannot be opened is unknown, not
    // ended, until an open succeeds.
    #[test]
    fn a_failed_open_is_unknown_until_resolved() {
        let mut table = Table::default();
        table.processes.insert(4, (true, 1, false));
        table.denied.push(4);
        let mut ledger = Ledger::default();
        ledger.reported(4, &mut table);
        assert_eq!(ledger.counted(), 0);
        assert_eq!(
            settle(&mut ledger, &mut table, 1, 0),
            Verdict::Pending("a named member has not ended")
        );
        table.denied.clear();
        assert_eq!(
            settle(&mut ledger, &mut table, 1, 0),
            Verdict::Pending("a named member has not ended")
        );
        table.processes.get_mut(&4).expect("4").2 = true;
        assert_eq!(settle(&mut ledger, &mut table, 1, 0), Verdict::Settled);
    }

    // A long life's ended members are kept as records (for duplicates), but
    // not visited, and their handles are closed as soon as they are seen
    // signalled.
    #[test]
    fn ended_members_are_pruned_from_the_open_set() {
        let mut table = Table::default();
        let mut ledger = Ledger::default();
        for pid in 1..=100u32 {
            table
                .processes
                .insert(pid * 4, (true, u64::from(pid), true));
            ledger.reported(pid * 4, &mut table);
        }
        table.processes.insert(404, (true, 101, false));
        ledger.reported(404, &mut table);
        assert_eq!(ledger.open_members(), 101);
        assert_eq!(ledger.check(&mut table), Ok(1));
        assert_eq!(ledger.open_members(), 1);
        assert_eq!(ledger.counted(), 101);
    }

    // Past the record cap a new pid is refused, never dropped silently: the
    // account can no longer settle.
    #[test]
    fn a_pid_past_the_cap_is_a_contradiction() {
        let mut table = Table::default();
        let mut ledger = Ledger::<u64>::default();
        for pid in 1..=u32::try_from(MAX_RECORDS).expect("cap fits") {
            ledger.records.insert(pid, Record::Vanished);
        }
        ledger.counted = MAX_RECORDS as u64;
        // A pid already kept is still handled.
        ledger.reported(4, &mut table);
        assert!(ledger.contradiction.is_none());
        ledger.reported(u32::MAX, &mut table);
        assert!(matches!(
            ledger.verdict(ledger.counted as u32, ledger.counted as u32, 0, Ok(0)),
            Verdict::Contradiction(_)
        ));
    }

    // A member whose creation time cannot be read (as a sandboxed or
    // rundown process may refuse a query) is still held and awaited, never
    // read as ended; once it has ended, a later process with its pid cannot
    // be told from it, so that one counts nothing.
    #[test]
    fn a_member_without_a_creation_time_is_awaited_not_ended() {
        let mut table = Table::default();
        table.processes.insert(4, (true, 1, false));
        table.untimed.push(1);
        let mut ledger = Ledger::default();
        ledger.reported(4, &mut table);
        assert_eq!(ledger.counted(), 1);
        assert_eq!(
            settle(&mut ledger, &mut table, 1, 0),
            Verdict::Pending("a named member has not ended")
        );
        table.processes.get_mut(&4).expect("4").2 = true;
        assert_eq!(settle(&mut ledger, &mut table, 1, 0), Verdict::Settled);
        table.processes.insert(4, (true, 2, false));
        ledger.reported(4, &mut table);
        assert_eq!(ledger.counted(), 1);
        assert_eq!(
            settle(&mut ledger, &mut table, 2, 0),
            Verdict::Pending("fewer members named than the job associated")
        );
    }

    #[test]
    fn a_handle_that_cannot_be_checked_is_not_ended() {
        let mut table = Table::default();
        table.processes.insert(4, (true, 1, true));
        table.unreadable.push(1);
        let mut ledger = Ledger::default();
        ledger.reported(4, &mut table);
        assert_eq!(
            settle(&mut ledger, &mut table, 1, 0),
            Verdict::Pending("a member's handle could not be checked")
        );
    }

    // Duplicate messages for one member count it once, so they cannot
    // stand in for a lost message.
    #[test]
    fn duplicate_messages_cannot_cover_a_lost_one() {
        let mut table = Table::default();
        table.processes.insert(4, (true, 1, true));
        table.processes.insert(8, (true, 2, true));
        let mut ledger = Ledger::default();
        ledger.reported(4, &mut table);
        ledger.reported(4, &mut table);
        assert_eq!(ledger.counted(), 1);
        assert_eq!(ledger.check(&mut table), Ok(0));
        // After the member ended and its handle closed, its pid may be
        // reported again: still the same member.
        ledger.reported(4, &mut table);
        assert_eq!(ledger.counted(), 1);
        assert_eq!(
            settle(&mut ledger, &mut table, 2, 0),
            Verdict::Pending("fewer members named than the job associated")
        );
        // A vanished pid reported twice also counts once.
        table.processes.remove(&4);
        let mut ledger = Ledger::default();
        ledger.reported(4, &mut table);
        ledger.reported(4, &mut table);
        assert_eq!(ledger.counted(), 1);
    }

    // A pid reused inside the job by a member still running is another
    // member; one that ended before it was opened cannot be told from a
    // duplicate, so the account falls short rather than over.
    #[test]
    fn a_pid_reused_inside_the_job() {
        let mut table = Table::default();
        table.processes.insert(4, (true, 1, true));
        let mut ledger = Ledger::default();
        ledger.reported(4, &mut table);
        assert_eq!(ledger.check(&mut table), Ok(0));
        table.processes.insert(4, (true, 7, false));
        ledger.reported(4, &mut table);
        assert_eq!(ledger.counted(), 2);
        assert_eq!(
            settle(&mut ledger, &mut table, 2, 0),
            Verdict::Pending("a named member has not ended")
        );
        table.processes.get_mut(&4).expect("4").2 = true;
        assert_eq!(settle(&mut ledger, &mut table, 2, 0), Verdict::Settled);

        table.processes.remove(&4);
        ledger.reported(4, &mut table);
        assert_eq!(ledger.counted(), 2);
        assert_eq!(
            settle(&mut ledger, &mut table, 3, 0),
            Verdict::Pending("fewer members named than the job associated")
        );
    }

    // Bake: a short child that reused an ended member's pid, joined the job,
    // and released the pid before it was opened is told from a duplicate
    // message by the exit message queued between them (the idle-churn
    // shortfall `tests/windows_job_rundown.rs` observed).
    #[test]
    fn an_exit_message_tells_a_reused_pid_from_a_duplicate() {
        let mut table = Table::default();
        table.processes.insert(4, (true, 1, true));
        let mut ledger = Ledger::default();
        ledger.reported(4, &mut table);
        assert_eq!(ledger.check(&mut table), Ok(0));
        ledger.exited(4);
        // The next process with pid 4 has come and gone.
        table.processes.remove(&4);
        ledger.reported(4, &mut table);
        assert_eq!(ledger.counted(), 2);
        assert_eq!(settle(&mut ledger, &mut table, 2, 0), Verdict::Settled);
        // The exit is taken by that message: a repeat counts nothing.
        ledger.reported(4, &mut table);
        assert_eq!(ledger.counted(), 2);

        // A vanished record, and a pid now naming a process outside the job.
        let mut table = Table::default();
        let mut ledger = Ledger::default();
        ledger.reported(8, &mut table);
        ledger.exited(8);
        table.processes.insert(8, (false, 5, false));
        ledger.reported(8, &mut table);
        assert_eq!(ledger.counted(), 2);
        assert_eq!(settle(&mut ledger, &mut table, 2, 0), Verdict::Settled);
    }

    // Failure controls: without an exit message between them, or for a pid
    // never reported, a repeated report counts nothing.
    #[test]
    fn an_exit_message_counts_only_after_an_ended_record() {
        let mut table = Table::default();
        table.processes.insert(4, (true, 1, true));
        let mut ledger = Ledger::default();
        ledger.reported(4, &mut table);
        assert_eq!(ledger.check(&mut table), Ok(0));
        table.processes.remove(&4);
        ledger.reported(4, &mut table);
        assert_eq!(ledger.counted(), 1);

        let mut ledger = Ledger::default();
        ledger.exited(12);
        ledger.reported(12, &mut table);
        ledger.reported(12, &mut table);
        assert_eq!(ledger.counted(), 1);
    }

    // A first member gone before its report was opened, whose pid the next
    // member already holds: the report opens the second, and the exit
    // message between the two reports counts the first.
    #[test]
    fn a_report_that_opens_a_later_member_still_counts_both() {
        let mut table = Table::default();
        table.processes.insert(16, (true, 3, false));
        let mut ledger = Ledger::default();
        ledger.reported(16, &mut table);
        ledger.exited(16);
        ledger.reported(16, &mut table);
        assert_eq!(ledger.counted(), 2);
        assert_eq!(
            settle(&mut ledger, &mut table, 2, 0),
            Verdict::Pending("a named member has not ended")
        );
        table.processes.get_mut(&16).expect("16").2 = true;
        assert_eq!(settle(&mut ledger, &mut table, 2, 0), Verdict::Settled);
    }

    /// A small xorshift generator, so the simulation is deterministic.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }
    }

    enum Message {
        New(u32),
        Exit(u32),
    }

    /// A churning job over three pids, its messages queued in order and
    /// taken late, some repeated before their process's exit and, when
    /// `lossy`, some lost. A settled account always means every process
    /// ever created has ended, and the count never exceeds the processes
    /// associated. Without a lost message, the account settles once the
    /// tree has ended.
    fn simulate(seed: u64, lossy: bool) {
        const PIDS: [u32; 3] = [4, 8, 12];
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut table = Table::default();
        let mut ledger = Ledger::default();
        let mut queue = std::collections::VecDeque::new();
        let mut associated = 0u32;
        let mut created = 0u64;
        let take = |ledger: &mut Ledger<u64>, table: &mut Table, message| match message {
            Message::New(pid) => ledger.reported(pid, table),
            Message::Exit(pid) => ledger.exited(pid),
        };
        let poll = |ledger: &mut Ledger<u64>, table: &mut Table, settled: u32| {
            ledger.retry(table);
            let waiting = ledger.check(table);
            let active = table
                .processes
                .values()
                .filter(|(_, _, signalled)| !signalled)
                .count() as u32;
            let verdict = ledger.verdict(settled, settled, active, waiting);
            if verdict == Verdict::Settled {
                assert!(
                    table.processes.values().all(|(_, _, signalled)| *signalled),
                    "seed {seed}: settled with a process running"
                );
            }
            verdict
        };
        for _ in 0..300 {
            match rng.below(6) {
                // A process is created with a free pid.
                0 => {
                    let free: Vec<u32> = PIDS
                        .iter()
                        .copied()
                        .filter(|pid| !table.processes.contains_key(pid))
                        .collect();
                    if !free.is_empty() {
                        let pid = free[rng.below(free.len() as u64) as usize];
                        created += 1;
                        associated += 1;
                        table.processes.insert(pid, (true, created, false));
                        let copies = match rng.below(10) {
                            0 if lossy => 0,
                            1 => 2,
                            _ => 1,
                        };
                        for _ in 0..copies {
                            queue.push_back(Message::New(pid));
                        }
                    }
                }
                // A running process exits.
                1 | 2 => {
                    // Sorted: the table's order is random.
                    let mut running: Vec<u32> = table
                        .processes
                        .iter()
                        .filter(|(_, (_, _, signalled))| !signalled)
                        .map(|(pid, _)| *pid)
                        .collect();
                    running.sort_unstable();
                    if !running.is_empty() {
                        let pid = running[rng.below(running.len() as u64) as usize];
                        table.processes.get_mut(&pid).expect("running").2 = true;
                        if !lossy || rng.below(10) != 0 {
                            queue.push_back(Message::Exit(pid));
                        }
                    }
                }
                // An ended process no handle holds releases its pid.
                3 => {
                    let held: Vec<u64> = ledger
                        .records
                        .values()
                        .filter_map(|record| match record {
                            Record::Live { handle, .. } => Some(*handle),
                            _ => None,
                        })
                        .collect();
                    let mut releasable: Vec<u32> = table
                        .processes
                        .iter()
                        .filter(|(_, (_, at, signalled))| *signalled && !held.contains(at))
                        .map(|(pid, _)| *pid)
                        .collect();
                    releasable.sort_unstable();
                    if !releasable.is_empty() {
                        let pid = releasable[rng.below(releasable.len() as u64) as usize];
                        table.processes.remove(&pid);
                    }
                }
                // The account takes the oldest message.
                4 => {
                    if let Some(message) = queue.pop_front() {
                        take(&mut ledger, &mut table, message);
                    }
                }
                // A settle poll, only once every message is taken, as
                // `Job::settle` drains the port first.
                _ => {
                    if queue.is_empty() {
                        poll(&mut ledger, &mut table, associated);
                    }
                }
            }
            assert!(
                ledger.counted() <= u64::from(associated),
                "seed {seed}: counted {} of {associated}",
                ledger.counted()
            );
        }
        // The tree is terminated: every process ends, and the account
        // drains what is left.
        let mut running: Vec<u32> = table
            .processes
            .iter()
            .filter(|(_, (_, _, signalled))| !signalled)
            .map(|(pid, _)| *pid)
            .collect();
        running.sort_unstable();
        for pid in running {
            table.processes.get_mut(&pid).expect("running").2 = true;
            queue.push_back(Message::Exit(pid));
        }
        while let Some(message) = queue.pop_front() {
            take(&mut ledger, &mut table, message);
        }
        let verdict = poll(&mut ledger, &mut table, associated);
        assert!(ledger.counted() <= u64::from(associated), "seed {seed}");
        if !lossy {
            assert_eq!(verdict, Verdict::Settled, "seed {seed}");
        }
    }

    #[test]
    fn a_simulated_churn_settles_only_once_every_process_ended() {
        for seed in 1..=500 {
            simulate(seed, false);
            simulate(seed, true);
        }
    }

    // A pid now naming a process outside the job: the reported member
    // released it.
    #[test]
    fn a_pid_reused_outside_the_job_counts_as_ended() {
        let mut table = Table::default();
        table.processes.insert(4, (false, 9, false));
        let mut ledger = Ledger::default();
        ledger.reported(4, &mut table);
        assert_eq!(settle(&mut ledger, &mut table, 1, 0), Verdict::Settled);
    }

    #[test]
    fn a_count_that_moves_or_exceeds_never_settles() {
        let mut table = Table::default();
        table.processes.insert(4, (true, 1, true));
        let mut ledger = Ledger::default();
        ledger.reported(4, &mut table);
        let waiting = ledger.check(&mut table);
        // A process associated while the members were checked.
        assert_eq!(
            ledger.verdict(1, 2, 0, waiting),
            Verdict::Pending("the job associated more processes meanwhile")
        );
        assert!(matches!(
            ledger.verdict(0, 0, 0, Ok(0)),
            Verdict::Contradiction(_)
        ));
        assert!(matches!(
            ledger.verdict(2, 1, 0, Ok(0)),
            Verdict::Contradiction(_)
        ));
        assert_eq!(
            ledger.verdict(1, 1, 1, Ok(0)),
            Verdict::Pending("the job still counts an active process")
        );
    }

    #[test]
    fn a_held_pid_released_or_renamed_is_a_contradiction() {
        let mut table = Table::default();
        table.processes.insert(4, (true, 1, false));
        let mut ledger = Ledger::default();
        ledger.reported(4, &mut table);
        table.processes.insert(4, (true, 2, false));
        ledger.reported(4, &mut table);
        assert!(matches!(
            settle(&mut ledger, &mut table, 1, 0),
            Verdict::Contradiction(_)
        ));

        let mut ledger = Ledger::default();
        table.processes.insert(8, (true, 3, false));
        ledger.reported(8, &mut table);
        table.processes.remove(&8);
        ledger.reported(8, &mut table);
        assert!(matches!(
            ledger.verdict(1, 1, 0, Ok(0)),
            Verdict::Contradiction(_)
        ));
        assert!(matches!(
            Ledger::<u64>::default().verdict(0, 0, 0, Ok(0)),
            Verdict::Settled
        ));
        let mut ledger = Ledger::<u64>::default();
        ledger.reported(0, &mut table);
        assert!(matches!(
            ledger.verdict(1, 1, 0, Ok(0)),
            Verdict::Contradiction(_)
        ));
    }
}

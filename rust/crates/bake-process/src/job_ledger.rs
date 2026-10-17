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
//!   before it was opened, a failed association counted by the job) never
//!   settles; more is a contradiction that never settles either.
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
//! platform; the Windows host is in `tree.rs`.

use std::collections::HashMap;

/// What opening a reported pid found.
#[derive(Debug)]
pub(crate) enum Opened<H> {
    /// No process has this pid (`ERROR_INVALID_PARAMETER`). Read as the
    /// reported process having ended (see the module docs).
    NoSuchProcess,
    /// The pid names a process outside the job: the reported one released
    /// it, so it ended (see the module docs).
    OtherProcess,
    /// A process in the job, with its creation time, held by `handle`; a
    /// held handle keeps its pid from being reused.
    Member { created: u64, handle: H },
    /// Neither open nor membership nor creation time could be read (access
    /// denied, a failed query): unknown, never ended.
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
    Live { created: u64, handle: H },
    /// A counted member seen signalled; its handle is closed, so its pid
    /// may be reused, which the creation time tells apart.
    Ended { created: u64 },
    /// A counted pid that could not be opened or named another process.
    Vanished,
    /// Reported, but unknown so far: counts nothing until resolved.
    Unresolved,
}

/// The settlement state; see [`Ledger::verdict`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Every associated process was named and observed ended.
    Settled,
    /// Not yet: waiting on members, messages, or a stable count.
    Pending(&'static str),
    /// The account cannot be trusted again; never settles.
    Contradiction(&'static str),
}

/// The account of one job.
#[derive(Debug)]
pub(crate) struct Ledger<H> {
    /// One record per reported pid: bounded by distinct pids, not by how
    /// many processes the job ever had.
    records: HashMap<u32, Record<H>>,
    counted: u64,
    contradiction: Option<&'static str>,
}

impl<H> Default for Ledger<H> {
    fn default() -> Self {
        Self {
            records: HashMap::new(),
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

    /// Marks the account untrustworthy for good.
    pub(crate) fn contradict(&mut self, why: &'static str) {
        self.contradiction.get_or_insert(why);
    }

    /// Records one `JOB_OBJECT_MSG_NEW_PROCESS` naming `pid`.
    pub(crate) fn reported(&mut self, pid: u32, host: &mut impl Host<Handle = H>) {
        if pid == 0 {
            // No process is created with pid 0: not a message to trust.
            self.contradict("a new-process message named pid 0");
            return;
        }
        let opened = host.open(pid);
        let record = self.records.remove(&pid);
        let next = match (record, opened) {
            (None | Some(Record::Unresolved), Opened::NoSuchProcess | Opened::OtherProcess) => {
                self.counted += 1;
                Record::Vanished
            }
            (None | Some(Record::Unresolved), Opened::Member { created, handle }) => {
                self.counted += 1;
                Record::Live { created, handle }
            }
            (None | Some(Record::Unresolved), Opened::Unknown) => Record::Unresolved,
            // A held handle pins the pid: it can only name the same member.
            (Some(Record::Live { created, handle }), Opened::Member { created: now, .. }) => {
                if now != created {
                    self.contradict("a held member's pid named another member");
                }
                Record::Live { created, handle }
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
            ) => {
                if now == created {
                    // The same, ended member named again.
                    Record::Ended { created }
                } else {
                    // A different member reusing the pid.
                    self.counted += 1;
                    Record::Live {
                        created: now,
                        handle,
                    }
                }
            }
            (Some(Record::Vanished), Opened::Member { created, handle }) => {
                // The vanished one released the pid before this member
                // existed (see the module docs), so this is another one.
                self.counted += 1;
                Record::Live { created, handle }
            }
            // A pid seen before that cannot be told apart from an earlier
            // report: counted nothing, so the account can only fall short.
            (Some(seen @ (Record::Ended { .. } | Record::Vanished)), _) => seen,
        };
        self.records.insert(pid, next);
    }

    /// Opens again every reported pid still unknown.
    pub(crate) fn retry(&mut self, host: &mut impl Host<Handle = H>) {
        let unresolved: Vec<u32> = self
            .records
            .iter()
            .filter(|(_, record)| matches!(record, Record::Unresolved))
            .map(|(pid, _)| *pid)
            .collect();
        for pid in unresolved {
            self.reported(pid, host);
        }
    }

    /// Checks every live member's handle once, closing those signalled.
    /// Returns how many are still not signalled, or unresolved; `Err` when a
    /// handle cannot be checked.
    pub(crate) fn check(&mut self, host: &mut impl Host<Handle = H>) -> Result<usize, ()> {
        let mut waiting = 0;
        let mut failed = false;
        for record in self.records.values_mut() {
            match record {
                Record::Live { created, handle } => match host.signalled(handle) {
                    Some(true) => {
                        *record = Record::Ended { created: *created };
                    }
                    Some(false) => waiting += 1,
                    None => failed = true,
                },
                Record::Unresolved => waiting += 1,
                Record::Ended { .. } | Record::Vanished => {}
            }
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
                    created: *created,
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

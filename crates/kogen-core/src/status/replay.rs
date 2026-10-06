//! Guaranteed status transition used by production derivation and xspec replay.

use super::model::{
    DependencyState, IntentFacts, StatusRun, classify_facts, dependency_reason, is_status,
};
use serde::Serialize;
use std::collections::BTreeMap;

mod observe;

#[derive(Clone, Debug, PartialEq)]
pub struct StatusRow {
    pub slug: String,
    pub status: String,
    pub priority: i64,
    pub at: i64,
    pub blocks: String,
    pub sched: String,
    pub started: i64,
    pub index: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StatusRaw {
    pub slug: String,
    pub trailer: bool,
    pub claimed: bool,
    pub run_status: String,
    pub event: String,
    pub alive: bool,
    pub approved: bool,
    pub reason: String,
    pub same: bool,
    pub blocks: String,
    pub priority: i64,
    pub at: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StatusEvent {
    Row(StatusRow),
    Raw(StatusRaw),
    Derive,
    Now(i64),
    Older(i64),
    Queue(bool),
    Agents(i64),
    Watch(String),
    Json,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StatusObservation {
    pub last: String,
    pub alpha: String,
    pub bravo: String,
    pub charlie: String,
    pub queue: Vec<String>,
    pub why_a: String,
    pub why_b: String,
    pub why_c: String,
    pub sections: Vec<String>,
    pub earlier: i64,
    pub elapsed: String,
    pub queue_line: String,
    pub next: String,
    pub next_priority: i64,
    pub next_dependencies: String,
    pub landed_shown: i64,
    pub watch_slug: String,
    pub watch_status: String,
    pub watch_position: i64,
    pub watch_queue_size: i64,
    pub exit: i64,
    pub json_detail: bool,
}

#[derive(Clone, Debug)]
struct Card {
    slug: String,
    status: String,
    priority: i64,
    at: i64,
    blocks: String,
    sched: String,
    started: i64,
    index: i64,
    trailer: bool,
    claimed: bool,
    run_status: String,
    event: String,
    alive: bool,
    approved: bool,
    reason: String,
    same: bool,
    from_raw: bool,
}

impl Default for Card {
    fn default() -> Self {
        Self {
            slug: String::new(),
            status: String::new(),
            priority: 0,
            at: 0,
            blocks: String::new(),
            sched: String::new(),
            started: 0,
            index: 0,
            trailer: false,
            claimed: false,
            run_status: String::new(),
            event: String::new(),
            alive: true,
            approved: false,
            reason: String::new(),
            same: true,
            from_raw: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct StatusReplay {
    cards: BTreeMap<String, Card>,
    running: bool,
    now: i64,
    older: i64,
    watch_slug: String,
    agents_busy: i64,
    exit: i64,
    last: String,
}

impl StatusReplay {
    #[must_use]
    pub fn new() -> Self {
        Self {
            last: "ok".to_owned(),
            ..Self::default()
        }
    }

    pub fn apply(&mut self, event: StatusEvent) -> StatusObservation {
        match event {
            StatusEvent::Row(row) => self.row(row),
            StatusEvent::Raw(raw) => self.raw(raw),
            StatusEvent::Derive => {
                let cards = self.cards.clone();
                for card in cards.values().filter(|card| card.from_raw) {
                    let slug = card.slug.clone();
                    let next = Self::raw_status(card);
                    if let Some(saved) = self.cards.get_mut(&slug) {
                        saved.status = next;
                    }
                }
                self.last = "ok".to_owned();
                self.view();
            }
            StatusEvent::Now(now) => {
                self.now = now;
                self.last = "ok".to_owned();
            }
            StatusEvent::Older(count) => {
                if count < 0 {
                    self.last = "bad_older".to_owned();
                } else {
                    self.older = count;
                    self.last = "ok".to_owned();
                }
            }
            StatusEvent::Queue(running) => {
                self.running = running;
                self.last = "ok".to_owned();
            }
            StatusEvent::Agents(busy) => {
                if busy < 0 {
                    self.last = "bad_agents".to_owned();
                } else {
                    self.agents_busy = busy;
                    self.last = "ok".to_owned();
                }
            }
            StatusEvent::Watch(slug) => {
                self.view();
                self.watch_slug = slug;
                self.exit = self.watch_exit();
                self.last = "ok".to_owned();
            }
            StatusEvent::Json => {
                self.last = "ok".to_owned();
            }
        }
        self.observe()
    }

    fn row(&mut self, row: StatusRow) {
        if !known(&row.slug) {
            self.last = "bad_slug".to_owned();
            return;
        }
        if !is_status(&row.status) {
            self.last = "bad_status".to_owned();
            return;
        }
        if !row.blocks.is_empty()
            && row.blocks != "BAD"
            && row.blocks != "ghost"
            && !known(&row.blocks)
        {
            self.last = "bad_blocks".to_owned();
            return;
        }
        let card = self.cards.entry(row.slug.clone()).or_default();
        card.slug = row.slug;
        card.status = row.status;
        card.priority = row.priority;
        card.at = row.at;
        card.blocks = row.blocks;
        card.sched = row.sched;
        card.started = row.started;
        card.index = row.index;
        card.from_raw = false;
        self.last = "ok".to_owned();
        self.view();
    }

    fn raw(&mut self, raw: StatusRaw) {
        if !known(&raw.slug) {
            self.last = "bad_slug".to_owned();
            return;
        }
        if !raw.blocks.is_empty()
            && raw.blocks != "BAD"
            && raw.blocks != "ghost"
            && !known(&raw.blocks)
        {
            self.last = "bad_blocks".to_owned();
            return;
        }
        let card = self.cards.entry(raw.slug.clone()).or_default();
        card.slug = raw.slug;
        card.trailer = raw.trailer;
        card.claimed = raw.claimed;
        card.run_status = raw.run_status;
        card.event = raw.event;
        card.alive = raw.alive;
        card.approved = raw.approved;
        card.reason = raw.reason;
        card.same = raw.same;
        card.blocks = raw.blocks;
        card.priority = raw.priority;
        card.at = raw.at;
        card.status = Self::raw_status(card);
        card.from_raw = true;
        self.last = "ok".to_owned();
        self.view();
    }

    fn raw_status(card: &Card) -> String {
        let approval = if card.approved {
            Some("current".to_owned())
        } else {
            None
        };
        let latest_run = (!card.run_status.is_empty()).then(|| StatusRun {
            run_id: "r".to_owned(),
            approval_commit: if card.same { "current" } else { "old" }.to_owned(),
            status: card.run_status.clone(),
            reason: card.reason.clone(),
            last_event: card.event.clone(),
            owner_alive: card.alive,
            same_approval: card.same,
            ..StatusRun::default()
        });
        let facts = IntentFacts {
            slug: card.slug.clone(),
            approval_commit: approval,
            landed_sha: card.trailer.then(|| "landed".to_owned()),
            latest_run,
            claimed_run_id: card.claimed.then(|| "r".to_owned()),
            ..IntentFacts::default()
        };
        classify_facts(&facts).as_str().to_owned()
    }

    fn view(&mut self) {
        let entries = self
            .cards
            .values()
            .filter(|card| !card.status.is_empty())
            .map(|card| {
                (
                    card.slug.clone(),
                    DependencyState {
                        status: card.status.clone(),
                        dependencies: if card.blocks.is_empty() {
                            Vec::new()
                        } else {
                            vec![card.blocks.clone()]
                        },
                        scheduling_error: (!card.sched.is_empty()).then(|| card.sched.clone()),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let reasons = self
            .cards
            .values()
            .filter(|card| card.status == "approved" || card.status == "blocked")
            .map(|card| (card.slug.clone(), dependency_reason(&entries, &card.slug)))
            .collect::<BTreeMap<_, _>>();
        for (slug, reason) in reasons {
            if let Some(card) = self.cards.get_mut(&slug) {
                card.status = if reason.is_some() {
                    "blocked"
                } else {
                    "approved"
                }
                .to_owned();
            }
        }
    }
}

fn known(slug: &str) -> bool {
    matches!(slug, "alpha" | "bravo" | "charlie")
}

//! Observation and watch calculations for status replay.

use super::*;

impl StatusReplay {
    #[must_use]
    pub fn observe(&self) -> StatusObservation {
        let mut shown = self.clone();
        shown.view();
        let queue = shown.queue();
        let landed = shown.count("landed") + shown.older;
        let status = |slug: &str| {
            shown
                .cards
                .get(slug)
                .map_or("", |card| card.status.as_str())
                .to_owned()
        };
        let next = queue.first().cloned().unwrap_or_default();
        let next_card = shown.cards.get(&next);
        let qlen = queue.len() as i64;
        let watch_status = if self.watch_slug.is_empty() {
            String::new()
        } else if self.exit == 2 {
            "not_found".to_owned()
        } else if status(&self.watch_slug) == "approved" {
            "queued".to_owned()
        } else {
            status(&self.watch_slug)
        };
        StatusObservation {
            last: self.last.clone(),
            alpha: status("alpha"),
            bravo: status("bravo"),
            charlie: status("charlie"),
            queue,
            why_a: shown.why("alpha"),
            why_b: shown.why("bravo"),
            why_c: shown.why("charlie"),
            sections: shown.sections(qlen),
            earlier: (landed - 5).max(0),
            elapsed: shown.elapsed(),
            queue_line: if shown.running {
                "running"
            } else if qlen > 0 {
                "waiting"
            } else {
                "stopped"
            }
            .to_owned(),
            next: next.clone(),
            next_priority: next_card.map_or(0, |card| card.priority),
            next_dependencies: next_card.map_or_else(String::new, |card| {
                if card.blocks.is_empty() {
                    "no_dependencies".to_owned()
                } else {
                    "dependencies_delivered".to_owned()
                }
            }),
            landed_shown: landed.min(5),
            watch_slug: self.watch_slug.clone(),
            watch_status,
            watch_position: shown.queue_position(&self.watch_slug),
            watch_queue_size: qlen,
            exit: self.exit,
            json_detail: false,
        }
    }

    fn why(&self, slug: &str) -> String {
        let Some(card) = self
            .cards
            .get(slug)
            .filter(|card| card.status == "approved" || card.status == "blocked")
        else {
            return String::new();
        };
        if !card.sched.is_empty() {
            return card.sched.clone();
        }
        if card.blocks.is_empty() {
            return String::new();
        }
        let entries = self
            .cards
            .values()
            .filter(|item| !item.status.is_empty())
            .map(|item| {
                (
                    item.slug.clone(),
                    DependencyState {
                        status: item.status.clone(),
                        dependencies: if item.blocks.is_empty() {
                            vec![]
                        } else {
                            vec![item.blocks.clone()]
                        },
                        scheduling_error: (!item.sched.is_empty()).then(|| item.sched.clone()),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        dependency_reason(&entries, slug).unwrap_or_default()
    }

    fn queue(&self) -> Vec<String> {
        let mut queue = self
            .cards
            .values()
            .filter(|card| card.status == "approved")
            .collect::<Vec<_>>();
        queue.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then(left.at.cmp(&right.at))
                .then(left.slug.cmp(&right.slug))
        });
        queue.into_iter().map(|card| card.slug.clone()).collect()
    }

    fn sections(&self, queued: i64) -> Vec<String> {
        let mut sections = Vec::new();
        for (name, status) in [
            ("Building", "building"),
            ("Queued", ""),
            ("Blocked", "blocked"),
            ("Failed", "failed"),
            ("Parked", "parked"),
            ("Interrupted", "interrupted"),
            ("Drafts", "draft"),
        ] {
            let count = if status.is_empty() {
                queued
            } else {
                self.count(status)
            };
            if count > 0 {
                sections.push(name.to_owned());
            }
        }
        if self.count("landed") + self.older > 0 {
            sections.push("Landed".to_owned());
        }
        sections
    }

    fn count(&self, status: &str) -> i64 {
        self.cards
            .values()
            .filter(|card| card.status == status)
            .count() as i64
    }

    fn elapsed(&self) -> String {
        let Some(card) = self
            .cards
            .values()
            .find(|card| card.status == "building" && card.started != 0)
        else {
            return String::new();
        };
        let seconds = self.now - card.started;
        if seconds < 60 {
            "s".to_owned()
        } else if seconds < 3600 {
            "m".to_owned()
        } else {
            "h".to_owned()
        }
    }

    fn queue_position(&self, slug: &str) -> i64 {
        self.queue()
            .iter()
            .position(|item| item == slug)
            .map_or(-1, |index| index as i64)
    }

    pub(super) fn watch_exit(&self) -> i64 {
        if !self.watch_slug.is_empty()
            && (!known(&self.watch_slug)
                || self
                    .cards
                    .get(&self.watch_slug)
                    .is_none_or(|card| card.status.is_empty()))
        {
            return 2;
        }
        if self.watch_slug.is_empty() {
            return if !self.running && self.count("building") == 0 && self.agents_busy == 0 {
                0
            } else {
                -1
            };
        }
        if self
            .cards
            .get(&self.watch_slug)
            .is_some_and(|card| card.status == "landed")
        {
            0
        } else {
            1
        }
    }
}

//! The presentation model behind an Apple Messages-style sidebar preview card.
//!
//! A card is three short lines plus an avatar: a title (`repo ⎇ branch`), a
//! status line derived from real git/PR/CI state, and a diffstat, with a
//! Messages-style relative timestamp in the corner. All of that is decided
//! here, as strings and small enums, so the element tree that paints it stays a
//! thin layout shell.
//!
//! Everything is gpui-free: plain data, `std`, and reads of
//! [`crate::git_context::GitContext`]. That is what keeps the wording and the
//! date arithmetic testable without a repo, a clock, or a network.

// The consumer of this module (the sidebar element tree) lands in a later
// pass; until then every item here is legitimately unused.
#![allow(dead_code)]

use std::time::{SystemTime, UNIX_EPOCH};

use crate::git::DirtyStats;
use crate::git_context::{GitContext, PrRollup, derive_rollup};

/// Which of the four avatar treatments a card shows.
///
/// The sidebar mock draws one glyph per bucket, not one per rollup variant: a
/// branch has a PR or it does not, and a PR is draft, live, or done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardAvatar {
    /// No pull request for this branch yet.
    NoPr,
    /// A draft pull request.
    Draft,
    /// A live pull request, whatever its review or CI weather.
    Open,
    /// A pull request that has left the board.
    Merged,
}

/// Collapse a [`PrRollup`] to the avatar its card should draw.
///
/// Note that `PrRollup::Finished` covers closed-without-merge as well as
/// merged, so those two share the `Merged` avatar. That is intentional: the
/// status line spells out which one it was, and a fifth avatar state would buy
/// the sidebar nothing.
pub fn avatar_for(rollup: PrRollup, is_draft: bool) -> CardAvatar {
    match rollup {
        PrRollup::None => CardAvatar::NoPr,
        PrRollup::Draft => CardAvatar::Draft,
        PrRollup::Finished => CardAvatar::Merged,
        // Draftness is threaded separately because derive_rollup deliberately
        // lets failing checks and conflicts take precedence over Draft.
        PrRollup::OpenCommented
        | PrRollup::OpenReadyToMerge
        | PrRollup::OpenApproved
        | PrRollup::OpenNeedsReview
        | PrRollup::OpenPending
        | PrRollup::ChecksFailing
        | PrRollup::ChecksPending
        | PrRollup::MergeConflicts => {
            if is_draft { CardAvatar::Draft } else { CardAvatar::Open }
        },
    }
}

/// Render `n` with comma thousands separators: `1204` -> `"1,204"`.
///
/// h20 prints diffstats without separators; the pwrde mock wants `+1,204`, so
/// this formatter is ours alone and carries its own test.
pub fn thousands(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    // Walk from the left, inserting a comma every time the number of digits
    // still to come is a multiple of three.
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A civil (proleptic Gregorian) date, as produced by [`civil_from_days`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CivilDate {
    year: i64,
    month: u32,
    day: u32,
}

/// Seconds since the UNIX epoch, negative for instants before it.
///
/// `duration_since` reports pre-epoch instants as an error carrying the
/// backwards distance, so we fold that back into a signed count instead of
/// losing it.
fn epoch_seconds(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// Seconds to add to a UTC instant to land on the viewer's wall clock.
///
/// `localtime_r` rather than a fixed offset because `tm_gmtoff` already folds
/// in whichever DST rule was in force *at that instant* — a card stamped in
/// July still buckets correctly when read in December.
fn local_offset(t: SystemTime) -> i64 {
    let clock = epoch_seconds(t) as libc::time_t;
    // SAFETY: `localtime_r` writes through the out-pointer and reads nothing
    // else; a zeroed `tm` is a valid destination. The reentrant form is used so
    // this stays sound if a background refresh ever formats a stamp.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&clock, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff as i64
    }
}

/// Days since 1970-01-01 *in local time*, flooring so pre-epoch instants keep
/// counting down.
///
/// Local rather than UTC because the buckets below are wall-clock ideas: west
/// of Greenwich a stamp from 8pm is already "tomorrow" in UTC, so a card
/// touched this evening would read `Yesterday`.
fn epoch_days(t: SystemTime) -> i64 {
    (epoch_seconds(t) + local_offset(t)).div_euclid(86_400)
}

/// Seconds elapsed since local midnight on the instant's own day, so the
/// same-day clock face reads as the viewer's wall clock.
fn seconds_of_day(t: SystemTime) -> i64 {
    (epoch_seconds(t) + local_offset(t)).rem_euclid(86_400)
}

/// Turn a day number into a civil date (Howard Hinnant's `civil_from_days`).
///
/// The trick is to shift the era so March is the first month: leap day then
/// lands at the end of a year and the day-of-year arithmetic becomes a pair of
/// integer divisions with no month table and no branching on leap years.
fn civil_from_days(z: i64) -> CivilDate {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096], day of era
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365], March-based
    let mp = (5 * doy + 2) / 153; // [0, 11], March = 0
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    CivilDate {
        year: y + i64::from(m <= 2),
        month: m as u32,
        day: d as u32,
    }
}

/// Day of the week for a day number, `0` = Sunday.
///
/// Epoch day 0 (1970-01-01) was a Thursday, hence the `+ 4`.
fn weekday_from_days(z: i64) -> u32 {
    (z + 4).rem_euclid(7) as u32
}

/// The English name of a weekday index from [`weekday_from_days`].
fn weekday_name(weekday: u32) -> &'static str {
    match weekday {
        0 => "Sunday",
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        _ => "Saturday",
    }
}

/// A 12-hour clock time like `1:40 PM`, with no leading zero on the hour.
fn clock_time(secs_of_day: i64) -> String {
    let hour24 = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let suffix = if hour24 < 12 { "AM" } else { "PM" };
    let hour12 = match hour24 % 12 {
        0 => 12,
        h => h,
    };
    format!("{hour12}:{minute:02} {suffix}")
}

/// A short absolute date like `8/24/26`: no zero padding on month or day, and
/// a two-digit year.
fn short_date(date: CivilDate) -> String {
    let yy = date.year.rem_euclid(100);
    format!("{}/{}/{:02}", date.month, date.day, yy)
}

/// Render `then` the way Messages stamps a conversation, relative to `now`.
///
/// The ladder is the spec's: a same-day time (`1:40 PM`), then `Yesterday`,
/// then a weekday name (`Tuesday`) for the rest of the past week, then a short
/// absolute date (`8/24/26`). Buckets are counted in whole civil days, not
/// elapsed seconds, so a stamp from 11pm reads `Yesterday` at 1am and not
/// "2 hours ago".
///
/// `now` is a parameter rather than a `SystemTime::now()` call so the ladder is
/// testable. All arithmetic is UTC — pwrde has no timezone database and is not
/// adding a date crate for one — so the day boundary is midnight UTC.
pub fn relative_time(then: SystemTime, now: SystemTime) -> String {
    ladder(epoch_days(then), seconds_of_day(then), epoch_days(now))
}

/// The bucket ladder itself, over local day numbers already resolved by
/// [`relative_time`].
///
/// Split out so it can be tested in any timezone: the caller owns the one
/// conversion that depends on the machine's clock settings, and everything
/// below here is arithmetic.
fn ladder(day_then: i64, secs_of_day_then: i64, day_now: i64) -> String {
    match day_now - day_then {
        // Today, including a stamp slightly in the future from a clock skew.
        0 => clock_time(secs_of_day_then),
        1 => "Yesterday".to_string(),
        2..=6 => weekday_name(weekday_from_days(day_then)).to_string(),
        // A week or more ago — and anything genuinely in the future, which
        // deserves a date rather than a lie about the past.
        _ => short_date(civil_from_days(day_then)),
    }
}

/// The three pieces of a card's diffstat row, already formatted.
///
/// They are kept apart rather than pre-joined because the element tree colours
/// the additions and the deletions differently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffstatLine {
    /// Added lines, e.g. `+1,204`.
    pub added: String,
    /// Removed lines, e.g. `-87`.
    pub removed: String,
    /// Touched files, e.g. `18 files` (or `1 file`).
    pub files: String,
    /// Trailing note when work is still uncommitted, e.g. `3 uncommitted`.
    ///
    /// Separate from the counts above because those describe the branch and
    /// this describes the working tree — two different facts, and the element
    /// tree tints them differently.
    pub uncommitted: Option<String>,
}

/// The diffstat row for a card, or `None` when there is nothing to show.
///
/// `None` is how the caller learns to paint the spec's `no code changes`
/// degradation, so this returns it for a non-repo, a repo with no stats, and a
/// repo whose working tree is clean.
///
/// The numbers come from `ctx.dirty`. A PR's own additions/deletions would be
/// preferable, but `gh::PrSummary` — what `GitContext` carries — does not have
/// them and the sidebar does not fetch per-PR detail. So the working tree is
/// the source, PR or no PR.
///
/// The minus sign is ASCII `-`; the mock's typographic `−` is a rendering
/// choice and belongs to the element tree, not to the model.
pub fn diffstat_line(ctx: &GitContext) -> Option<DiffstatLine> {
    if !ctx.is_git {
        return None;
    }

    let nonzero = |s: &DirtyStats| s.files > 0 || s.insertions > 0 || s.deletions > 0;
    let dirty = ctx.dirty.filter(nonzero);

    // The branch's committed work is what "changes on this branch" means, so it
    // owns the counts. Falling back to the working tree matters for a branch
    // whose base cannot be resolved (no remote yet) — there, uncommitted work is
    // the only work there is, and reporting nothing would be a lie by omission.
    let (counts, dirty_is_the_count) = match ctx.branch_diff.filter(nonzero) {
        Some(branch) => (Some(branch), false),
        None => (dirty, true),
    };
    let counts = counts?;

    // Only a file count for the uncommitted note: the question a card answers is
    // "is there anything left to commit", not how much. Suppressed when the
    // working tree *is* the number already shown, which would double-count it.
    let uncommitted = dirty
        .filter(|_| !dirty_is_the_count)
        .map(|d| format!("{} uncommitted", d.files));

    Some(DiffstatLine {
        added: format!("+{}", thousands(counts.insertions)),
        removed: format!("-{}", thousands(counts.deletions)),
        files: files_phrase(counts.files),
        uncommitted,
    })
}

/// `18 files`, `1 file`, `0 files` — pluralized, thousands-separated.
fn files_phrase(files: u32) -> String {
    let noun = if files == 1 { "file" } else { "files" };
    format!("{} {}", thousands(files), noun)
}

/// How a rollup's words sit around the PR number.
///
/// Every phrase the status line can say about a PR lives in [`pr_phrase`],
/// and this enum records the only two shapes those phrases take, so the
/// actual `format!` happens once instead of once per variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrPhrase {
    /// Rendered as `<words> #<number>` — the words lead, the number trails.
    Lead(&'static str),
    /// Rendered as `PR #<number><words>` — the number leads.
    Trail(&'static str),
}

/// The exact words each rollup contributes to the status line.
///
/// `merged` distinguishes the two [`PrRollup::Finished`] readings: the rollup
/// covers merged and closed-without-merge alike (see [`avatar_for`]), and only
/// the PR's own state can tell them apart.
fn pr_phrase(rollup: PrRollup, merged: bool) -> PrPhrase {
    use PrPhrase::{Lead, Trail};
    match rollup {
        // `None` never reaches here: `status_line` answers it from the working
        // tree before it asks for a phrase.
        PrRollup::None => Lead("PR"),
        PrRollup::Draft => Lead("Draft PR"),
        PrRollup::Finished => {
            if merged {
                Lead("Merged ✓")
            } else {
                Lead("Closed")
            }
        }
        PrRollup::ChecksFailing => Trail(" · checks failing"),
        PrRollup::ChecksPending => Trail(" · checks running"),
        PrRollup::MergeConflicts => Trail(" · merge conflicts"),
        PrRollup::OpenNeedsReview => Trail(" · needs review"),
        PrRollup::OpenApproved => Trail(" · approved"),
        PrRollup::OpenReadyToMerge => Trail(" · ready to merge"),
        PrRollup::OpenCommented => Trail(" · changes requested"),
        PrRollup::OpenPending => Trail(" open"),
    }
}

/// The words the working tree contributes when there is no PR at all.
const NO_PR: &str = "No PR yet";
/// What a detached HEAD says where a branch name would go.
const DETACHED_HEAD: &str = "Detached HEAD";
/// What the line says when the PR lookup itself failed.
const PR_UNAVAILABLE: &str = "PR status unavailable";

/// The card's middle line: what is going on with this checkout right now.
///
/// Everything here is derived from real git/PR/CI state — there is no activity
/// model in this repo, so the mock's "crunched for 1h 10m" style copy is
/// deliberately absent rather than stubbed.
///
/// Returns the empty string when the directory is not a repository at all; the
/// caller omits the line entirely in that case.
pub fn status_line(ctx: &GitContext) -> String {
    if !ctx.is_git {
        return String::new();
    }
    if ctx.branch.as_deref().unwrap_or("").is_empty() {
        return DETACHED_HEAD.to_string();
    }
    // A failed lookup is not the same as "no PR" — say so instead of guessing.
    if ctx.pr.is_none() && ctx.pr_error.is_some() {
        return PR_UNAVAILABLE.to_string();
    }
    let rollup = derive_rollup(ctx.pr.as_ref());
    if rollup == PrRollup::None {
        let files = ctx.dirty.map(|d| d.files).unwrap_or(0);
        return if files > 0 {
            uncommitted_phrase(files)
        } else {
            NO_PR.to_string()
        };
    }
    let Some(pr) = ctx.pr.as_ref() else {
        // A non-`None` rollup always comes from a PR; belt and braces.
        return NO_PR.to_string();
    };
    // The PR's own title, not a description of its state. The title is what
    // identifies the work; the state is already carried by the avatar's colour
    // and glyph, so spending this line on "Draft PR #3837" said the least
    // useful of the two things it could say.
    //
    // Falls back to the state phrasing when the title is empty — a cold `lfg`
    // cache can hand back a summary with a number but no title yet, and a blank
    // line would read as a card that failed to load.
    // Draft-ness rides along on the line because `derive_rollup` lets failing
    // checks and conflicts outrank `Draft`, and a draft that is merely broken
    // still reads very differently from one that is up for review. Skipped when
    // the rollup *is* `Draft`, whose phrasing already leads with the word, and
    // when the PR has left the board — "Draft · merged" is nonsense.
    let draft = pr.is_draft && !matches!(rollup, PrRollup::Draft | PrRollup::Finished);
    let title = pr.title.trim();
    if !title.is_empty() {
        return if draft { format!("Draft · {title}") } else { title.to_string() };
    }
    let merged = pr.state.eq_ignore_ascii_case("merged");
    let phrase = match pr_phrase(rollup, merged) {
        PrPhrase::Lead(words) => format!("{words} #{}", pr.number),
        PrPhrase::Trail(words) => format!("PR #{}{words}", pr.number),
    };
    if draft { format!("Draft · {phrase}") } else { phrase }
}

/// `1 uncommitted file` / `18 uncommitted files`, thousands-separated.
fn uncommitted_phrase(files: u32) -> String {
    let noun = if files == 1 { "file" } else { "files" };
    format!("{} uncommitted {noun}", thousands(files))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    use crate::gh::{Check, CheckStatus, PrSummary};
    use crate::git::DirtyStats;

    /// An instant `days` days and `secs` seconds after the epoch.
    fn at(days: i64, secs: i64) -> SystemTime {
        let total = days * 86_400 + secs;
        UNIX_EPOCH + Duration::from_secs(total as u64)
    }

    /// Seconds since midnight for a 24-hour clock time.
    fn hms(h: i64, m: i64) -> i64 {
        h * 3_600 + m * 60
    }

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(
            civil_from_days(0),
            CivilDate {
                year: 1970,
                month: 1,
                day: 1
            }
        );
        // Leap day 2020: 2020-02-29 is epoch day 18321.
        assert_eq!(
            civil_from_days(18_321),
            CivilDate {
                year: 2020,
                month: 2,
                day: 29
            }
        );
        assert_eq!(
            civil_from_days(18_322),
            CivilDate {
                year: 2020,
                month: 3,
                day: 1
            }
        );
        // Year boundary: 2019-12-31 -> 2020-01-01.
        assert_eq!(
            civil_from_days(18_261),
            CivilDate {
                year: 2019,
                month: 12,
                day: 31
            }
        );
        assert_eq!(
            civil_from_days(18_262),
            CivilDate {
                year: 2020,
                month: 1,
                day: 1
            }
        );
        // 1900 was not a leap year, 2000 was.
        assert_eq!(
            civil_from_days(11_016),
            CivilDate {
                year: 2000,
                month: 2,
                day: 29
            }
        );
        assert_eq!(
            civil_from_days(20_689),
            CivilDate {
                year: 2026,
                month: 8,
                day: 24
            }
        );
    }

    #[test]
    fn weekday_counts_from_a_thursday_epoch() {
        assert_eq!(weekday_name(weekday_from_days(0)), "Thursday");
        assert_eq!(weekday_name(weekday_from_days(1)), "Friday");
        assert_eq!(weekday_name(weekday_from_days(4)), "Monday");
        // 2020-02-29 was a Saturday, 2026-08-24 a Monday.
        assert_eq!(weekday_name(weekday_from_days(18_321)), "Saturday");
        assert_eq!(weekday_name(weekday_from_days(20_689)), "Monday");
    }

    #[test]
    fn clock_time_uses_a_twelve_hour_face() {
        assert_eq!(clock_time(hms(13, 40)), "1:40 PM");
        assert_eq!(clock_time(hms(0, 5)), "12:05 AM");
        assert_eq!(clock_time(hms(12, 0)), "12:00 PM");
        assert_eq!(clock_time(hms(9, 7)), "9:07 AM");
        assert_eq!(clock_time(hms(23, 59)), "11:59 PM");
    }

    #[test]
    fn short_date_is_month_day_two_digit_year() {
        assert_eq!(
            short_date(CivilDate {
                year: 2026,
                month: 8,
                day: 24
            }),
            "8/24/26"
        );
        assert_eq!(
            short_date(CivilDate {
                year: 2001,
                month: 1,
                day: 2
            }),
            "1/2/01"
        );
    }

    #[test]
    fn relative_time_walks_the_messages_ladder() {
        // "Now" is 2026-08-31 (a Monday) at 6:00 PM, local time. The ladder is
        // exercised directly so the assertions hold in any timezone; the
        // local-day conversion is covered by
        // `relative_time_buckets_by_local_day` below.
        let today = 20_696;
        let day = |d: i64, secs: i64| ladder(d, secs, today);

        // Same day: a clock time.
        assert_eq!(day(today, hms(13, 40)), "1:40 PM");
        // Midnight today is still today.
        assert_eq!(day(today, 0), "12:00 AM");
        // One civil day back, even a minute earlier, is Yesterday.
        assert_eq!(day(today - 1, hms(23, 59)), "Yesterday");
        assert_eq!(day(today - 1, 0), "Yesterday");
        // Two through six days back read as weekday names.
        assert_eq!(day(today - 2, hms(9, 0)), "Saturday");
        assert_eq!(day(today - 6, hms(9, 0)), "Tuesday");
        // Seven days back falls off the ladder into an absolute date.
        assert_eq!(day(today - 7, hms(9, 0)), "8/24/26");
        assert_eq!(day(today - 400, hms(9, 0)), "7/27/25");
        // A future stamp on the same day still shows its time; further out it
        // gets a date rather than a weekday.
        assert_eq!(day(today, hms(19, 30)), "7:30 PM");
        assert_eq!(day(today + 3, hms(9, 0)), "9/3/26");
    }

    #[test]
    fn relative_time_buckets_by_local_day() {
        // Whatever the machine's timezone, an instant and the same instant a
        // day earlier must land in adjacent buckets — this is what the UTC-only
        // arithmetic got wrong, misfiling an evening stamp as `Yesterday`.
        let now = SystemTime::now();
        let a_day = std::time::Duration::from_secs(86_400);
        assert_eq!(relative_time(now - a_day, now), "Yesterday");
        // And "now" is always today, which a UTC day boundary cannot promise.
        assert!(
            relative_time(now, now).ends_with("AM") || relative_time(now, now).ends_with("PM"),
            "a stamp of right now should read as a clock time, got {:?}",
            relative_time(now, now)
        );
    }

    #[test]
    fn avatar_buckets_every_rollup_variant() {
        assert_eq!(avatar_for(PrRollup::None, false), CardAvatar::NoPr);
        assert_eq!(avatar_for(PrRollup::Draft, true), CardAvatar::Draft);
        assert_eq!(avatar_for(PrRollup::Finished, false), CardAvatar::Merged);
        for open in [
            PrRollup::OpenCommented,
            PrRollup::OpenReadyToMerge,
            PrRollup::OpenApproved,
            PrRollup::OpenNeedsReview,
            PrRollup::OpenPending,
            PrRollup::ChecksFailing,
            PrRollup::ChecksPending,
            PrRollup::MergeConflicts,
        ] {
            assert_eq!(avatar_for(open, false), CardAvatar::Open, "{open:?}");
        }
    }

    #[test]
    fn draft_avatar_wins_over_live_rollup_but_not_finished() {
        assert_eq!(
            avatar_for(PrRollup::ChecksFailing, true),
            CardAvatar::Draft
        );
        assert_eq!(
            avatar_for(PrRollup::MergeConflicts, true),
            CardAvatar::Draft
        );
        assert_eq!(avatar_for(PrRollup::Finished, true), CardAvatar::Merged);
        assert_eq!(avatar_for(PrRollup::OpenReadyToMerge, false), CardAvatar::Open);
    }

    #[test]
    fn thousands_groups_by_three() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(87), "87");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1204), "1,204");
        assert_eq!(thousands(1_000_000), "1,000,000");
    }

    /// A git context for `pwrde` on branch `main`, with no stats and no PR.
    /// `GitContext::empty` is private to its module, so cards build one by
    /// hand; every field is public, which is what makes that cheap.
    fn ctx() -> GitContext {
        GitContext {
            is_git: true,
            cwd: PathBuf::from("/src/pwrde"),
            repo: Some("pwrde".to_string()),
            branch: Some("main".to_string()),
            default_branch: Some("main".to_string()),
            dirty: None,
            branch_diff: None,
            pr: None,
            pr_error: None,
            fetched_at: UNIX_EPOCH,
        }
    }

    /// A context for a directory that is not a repository at all.
    fn non_git_ctx() -> GitContext {
        GitContext {
            is_git: false,
            repo: None,
            branch: None,
            default_branch: None,
            ..ctx()
        }
    }

    #[test]
    fn diffstat_line_pluralizes_and_separates() {
        let mut c = ctx();
        c.dirty = Some(DirtyStats {
            files: 18,
            insertions: 1204,
            deletions: 87,
        });
        assert_eq!(
            diffstat_line(&c),
            Some(DiffstatLine {
                added: "+1,204".to_string(),
                removed: "-87".to_string(),
                files: "18 files".to_string(),
                // No base to compare against, so the working tree *is* the
                // branch's work — noting it as uncommitted too would
                // double-count it.
                uncommitted: None,
            })
        );

        c.dirty = Some(DirtyStats {
            files: 1,
            insertions: 3,
            deletions: 0,
        });
        assert_eq!(diffstat_line(&c).unwrap().files, "1 file");

        c.dirty = Some(DirtyStats {
            files: 1_000,
            insertions: 1_000_000,
            deletions: 2_500,
        });
        let stat = diffstat_line(&c).unwrap();
        assert_eq!(stat.added, "+1,000,000");
        assert_eq!(stat.removed, "-2,500");
        assert_eq!(stat.files, "1,000 files");
    }

    #[test]
    fn diffstat_line_counts_the_branch_and_flags_uncommitted_work() {
        // The case that motivated this: a PR with thousands of lines committed
        // and a clean tree used to read "no code changes", because the counts
        // came from `dirty` alone.
        let mut c = ctx();
        c.branch_diff = Some(DirtyStats {
            files: 65,
            insertions: 7311,
            deletions: 6,
        });
        c.dirty = Some(DirtyStats::default());
        let stat = diffstat_line(&c).expect("committed work must show");
        assert_eq!(stat.added, "+7,311");
        assert_eq!(stat.removed, "-6");
        assert_eq!(stat.files, "65 files");
        assert_eq!(stat.uncommitted, None, "a clean tree has nothing to flag");

        // Dirty tree on top of committed work: counts stay the branch's, and
        // the note says how many files are still local.
        c.dirty = Some(DirtyStats {
            files: 3,
            insertions: 12,
            deletions: 4,
        });
        let stat = diffstat_line(&c).expect("still shows");
        assert_eq!(stat.added, "+7,311", "counts describe the branch, not the tree");
        assert_eq!(stat.uncommitted.as_deref(), Some("3 uncommitted"));

        // No resolvable base (no remote yet): the working tree is all there is,
        // so it owns the counts and is not also flagged as uncommitted.
        let mut c = ctx();
        c.branch_diff = None;
        c.dirty = Some(DirtyStats {
            files: 2,
            insertions: 9,
            deletions: 1,
        });
        let stat = diffstat_line(&c).expect("falls back to the tree");
        assert_eq!(stat.added, "+9");
        assert_eq!(stat.files, "2 files");
        assert_eq!(stat.uncommitted, None);
    }

    #[test]
    fn diffstat_line_is_none_when_there_is_nothing_to_show() {
        // No stats gathered yet.
        assert_eq!(diffstat_line(&ctx()), None);

        // A clean tree with nothing committed on the branch either.
        let mut c = ctx();
        c.dirty = Some(DirtyStats::default());
        c.branch_diff = Some(DirtyStats::default());
        assert_eq!(diffstat_line(&c), None);

        // Not a repository, even if stale stats somehow survived.
        let mut c = non_git_ctx();
        c.dirty = Some(DirtyStats {
            files: 2,
            insertions: 5,
            deletions: 1,
        });
        assert_eq!(diffstat_line(&c), None);
    }

    /// An open PR with no review verdict, no mergeability answer and no checks.
    fn pr(number: u32, state: &str) -> PrSummary {
        PrSummary {
            number,
            title: "Treemap".to_string(),
            state: state.to_string(),
            is_draft: false,
            head: "feat/treemap".to_string(),
            author: "twhitehurst".to_string(),
            review_decision: None,
            mergeable: None,
            checks: Vec::new(),
            url: String::new(),
        }
    }

    /// One CI check in the given state.
    fn check(status: CheckStatus) -> Check {
        Check {
            name: "ci".to_string(),
            status,
            url: String::new(),
        }
    }

    /// A git context carrying `summary` as its PR.
    fn with_pr(summary: PrSummary) -> GitContext {
        GitContext {
            pr: Some(summary),
            ..ctx()
        }
    }

    #[test]
    fn status_line_reads_the_working_tree_when_there_is_no_pr() {
        // Not a repository: the caller omits the line entirely.
        assert_eq!(status_line(&non_git_ctx()), "");
        // Detached HEAD, whether the branch is missing or blank.
        assert_eq!(
            status_line(&GitContext {
                branch: None,
                ..ctx()
            }),
            "Detached HEAD"
        );
        assert_eq!(
            status_line(&GitContext {
                branch: Some(String::new()),
                ..ctx()
            }),
            "Detached HEAD"
        );
        // No PR, clean tree.
        assert_eq!(status_line(&ctx()), "No PR yet");
        assert_eq!(
            status_line(&GitContext {
                dirty: Some(DirtyStats::default()),
                branch_diff: None,
                ..ctx()
            }),
            "No PR yet"
        );
        // No PR, dirty tree — singular and plural.
        assert_eq!(
            status_line(&GitContext {
                dirty: Some(DirtyStats {
                    files: 1,
                    insertions: 4,
                    deletions: 0,
                }),
                ..ctx()
            }),
            "1 uncommitted file"
        );
        assert_eq!(
            status_line(&GitContext {
                dirty: Some(DirtyStats {
                    files: 1_204,
                    insertions: 9,
                    deletions: 2,
                }),
                ..ctx()
            }),
            "1,204 uncommitted files"
        );
        // A failed PR lookup is not the same as "no PR".
        assert_eq!(
            status_line(&GitContext {
                pr_error: Some("gh sign-in required".to_string()),
                dirty: Some(DirtyStats {
                    files: 3,
                    insertions: 1,
                    deletions: 1,
                }),
                ..ctx()
            }),
            "PR status unavailable"
        );
    }

    #[test]
    fn status_line_shows_the_pr_title_and_falls_back_to_its_state() {
        let draft = PrSummary {
            is_draft: true,
            ..pr(12, "open")
        };
        let failing = PrSummary {
            checks: vec![check(CheckStatus::Failure)],
            ..pr(12, "open")
        };
        let draft_failing = PrSummary {
            is_draft: true,
            checks: vec![check(CheckStatus::Failure)],
            ..pr(12, "open")
        };
        let draft_conflicts = PrSummary {
            is_draft: true,
            mergeable: Some("CONFLICTING".to_string()),
            ..pr(12, "open")
        };
        let pending = PrSummary {
            checks: vec![check(CheckStatus::Pending)],
            ..pr(12, "open")
        };
        let conflicts = PrSummary {
            mergeable: Some("CONFLICTING".to_string()),
            ..pr(12, "open")
        };
        let needs_review = PrSummary {
            review_decision: Some("REVIEW_REQUIRED".to_string()),
            ..pr(12, "open")
        };
        let approved = PrSummary {
            review_decision: Some("APPROVED".to_string()),
            mergeable: Some("UNKNOWN".to_string()),
            ..pr(12, "open")
        };
        let ready = PrSummary {
            review_decision: Some("APPROVED".to_string()),
            mergeable: Some("MERGEABLE".to_string()),
            checks: vec![check(CheckStatus::Success)],
            ..pr(12, "open")
        };
        let commented = PrSummary {
            review_decision: Some("CHANGES_REQUESTED".to_string()),
            ..pr(12, "open")
        };

        // Fourth field: whether the status line should lead with "Draft · ".
        // Written out per case on purpose — deriving it from the production
        // predicate would make every assertion below self-fulfilling.
        let cases: Vec<(PrSummary, PrRollup, &str, bool)> = vec![
            (draft, PrRollup::Draft, "Draft PR #12", false),
            (failing, PrRollup::ChecksFailing, "PR #12 · checks failing", false),
            (draft_failing, PrRollup::ChecksFailing, "PR #12 · checks failing", true),
            (draft_conflicts, PrRollup::MergeConflicts, "PR #12 · merge conflicts", true),
            (pending, PrRollup::ChecksPending, "PR #12 · checks running", false),
            (
                conflicts,
                PrRollup::MergeConflicts,
                "PR #12 · merge conflicts",
                false,
            ),
            (
                needs_review,
                PrRollup::OpenNeedsReview,
                "PR #12 · needs review",
                false,
            ),
            (approved, PrRollup::OpenApproved, "PR #12 · approved", false),
            (ready, PrRollup::OpenReadyToMerge, "PR #12 · ready to merge", false),
            (
                commented,
                PrRollup::OpenCommented,
                "PR #12 · changes requested",
                false,
            ),
            (pr(12, "open"), PrRollup::OpenPending, "PR #12 open", false),
            (pr(7, "merged"), PrRollup::Finished, "Merged ✓ #7", false),
            (pr(7, "closed"), PrRollup::Finished, "Closed #7", false),
            // A PR that leaves the board keeps `isDraft` on GitHub, so these
            // two pin the `Finished` half of the prefix guard: "Draft ·
            // Merged ✓" would be nonsense, and no other case reaches it.
            (
                PrSummary { is_draft: true, ..pr(7, "merged") },
                PrRollup::Finished,
                "Merged ✓ #7",
                false,
            ),
            (
                PrSummary { is_draft: true, ..pr(7, "closed") },
                PrRollup::Finished,
                "Closed #7",
                false,
            ),
        ];

        for (summary, rollup, phrasing, prefixed) in cases {
            // The rollup ladder still has to classify every one of these...
            let titled = with_pr(summary.clone());
            assert_eq!(
                derive_rollup(titled.pr.as_ref()),
                rollup,
                "rollup for {phrasing}"
            );

            // ...but the line itself shows the PR's title, whatever the state:
            // the avatar already carries the state, and the title is what
            // identifies the work.
            let titled_expected = if prefixed { "Draft · Treemap" } else { "Treemap" };
            assert_eq!(
                status_line(&titled),
                titled_expected,
                "titled PR should show its title, not {phrasing:?}"
            );

            // With no title to show — a cold cache can hand back a number and
            // nothing else — the state phrasing is the fallback, so a card
            // never renders a blank line.
            let untitled = with_pr(PrSummary {
                title: String::new(),
                ..summary
            });
            let expected = if prefixed {
                format!("Draft · {phrasing}")
            } else {
                phrasing.to_string()
            };
            assert_eq!(status_line(&untitled), expected);
        }
    }

}

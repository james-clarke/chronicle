#!/usr/bin/env python3
"""The site's week: five designed days for the fixtures cast (m43 chunk 0).

Writes `fixtures/site/<weekday>.jsonl`, one `Event` per line in the format
the golden fixtures use, for Sam's week of 7–11 September 2026. The days are
generated rather than typed so their shape is honest: window switches every
few minutes with the jitter a capture has, browser heartbeats beside browser
windows, breaks as AFK edges. Everything is seeded, so two runs write the
same bytes.

    python3 scripts/site_week.py            # regenerate the five files
    scripts/site-shots.sh                   # replays them and takes the shots

The cast is described in fixtures/README.md; every name here is invented.
"""

import json
import random
from datetime import datetime, timedelta
from pathlib import Path
from zoneinfo import ZoneInfo

TZ = ZoneInfo("America/New_York")
OUT = Path(__file__).resolve().parent.parent / "fixtures" / "site"

# --- windows ---------------------------------------------------------------
# (app, title, url) — app is the X11 WM_CLASS the collector reports; the url
# rides on a `url` heartbeat 2.5 s after the focus, like a browser extension.

TERM_MAILER = ("Terminator", "sam@workstation: ~/dev/mailer", None)
TERM_CONTOSO = ("Terminator", "sam@workstation: ~/dev/contoso", None)
TERM_HOME = ("Terminator", "sam@workstation: ~", None)

CODE_RETRY = ("Code", "sender.py — mailer — Visual Studio Code", None)
CODE_RETRY_TEST = ("Code", "test_sender.py — mailer — Visual Studio Code", None)
CODE_RENEWAL = ("Code", "renewals.py — contoso — Visual Studio Code", None)
CODE_RENEWAL_TPL = ("Code", "renewal_reminder.html — contoso — Visual Studio Code", None)
CODE_RENEWAL_TEST = ("Code", "test_renewals.py — contoso — Visual Studio Code", None)
CODE_TASKS = ("Code", "tasks.py — contoso — Visual Studio Code", None)
CODE_DEPLOY = ("Code", "deploy.yml — contoso — Visual Studio Code", None)
CODE_SETTINGS = ("Code", "settings.py — contoso — Visual Studio Code", None)

JIRA_11381 = (
    "Google-chrome",
    "[ACME-11381] mailer: retry backoff on SMS sends - Jira - Google Chrome",
    "https://acme.atlassian.net/browse/ACME-11381",
)
JIRA_11382 = (
    "Google-chrome",
    "[ACME-11382] contoso: membership renewal reminder emails - Jira - Google Chrome",
    "https://acme.atlassian.net/browse/ACME-11382",
)
JIRA_11390 = (
    "Google-chrome",
    "[ACME-11390] northwind: renewal batch times out nightly - Jira - Google Chrome",
    "https://acme.atlassian.net/browse/ACME-11390",
)
JIRA_11374 = (
    "Google-chrome",
    "[ACME-11374] contoso: staging deploys from a tag - Jira - Google Chrome",
    "https://acme.atlassian.net/browse/ACME-11374",
)
JIRA_BOARD = (
    "Google-chrome",
    "Platform board - Agile Board - Jira - Google Chrome",
    "https://acme.atlassian.net/jira/software/projects/ACME/boards/12",
)
PR_412 = (
    "Google-chrome",
    "Retry SMS sends with exponential backoff by sam · Pull Request #412 · acme/mailer - Google Chrome",
    "https://github.com/acme/mailer/pull/412",
)
PR_415 = (
    "Google-chrome",
    "Membership renewal reminder emails by sam · Pull Request #415 · acme/contoso - Google Chrome",
    "https://github.com/acme/contoso/pull/415",
)
PR_418 = (
    "Google-chrome",
    "Chunk the nightly renewal batch by sam · Pull Request #418 · acme/contoso - Google Chrome",
    "https://github.com/acme/contoso/pull/418",
)
PR_409 = (
    "Google-chrome",
    "Audit log for plan changes by priya · Pull Request #409 · acme/contoso - Google Chrome",
    "https://github.com/acme/contoso/pull/409",
)
PR_410 = (
    "Google-chrome",
    "Drop the legacy invoice export by tomasz · Pull Request #410 · acme/contoso - Google Chrome",
    "https://github.com/acme/contoso/pull/410",
)
PR_416 = (
    "Google-chrome",
    "Rate-limit the public signup endpoint by priya · Pull Request #416 · acme/contoso - Google Chrome",
    "https://github.com/acme/contoso/pull/416",
)
CI_CONTOSO = (
    "Google-chrome",
    "CI · contoso · run 2231 - GitHub - Google Chrome",
    "https://github.com/acme/contoso/actions/runs/2231",
)
CI_MAILER = (
    "Google-chrome",
    "CI · mailer · run 884 - GitHub - Google Chrome",
    "https://github.com/acme/mailer/actions/runs/884",
)
DJANGO_DOCS = (
    "Google-chrome",
    "Sending email | Django documentation - Google Chrome",
    "https://docs.djangoproject.com/en/5.2/topics/email/",
)
DJANGO_TPL = (
    "Google-chrome",
    "The Django template language | Django documentation - Google Chrome",
    "https://docs.djangoproject.com/en/5.2/ref/templates/language/",
)
CELERY_DOCS = (
    "Google-chrome",
    "Periodic Tasks — Celery 5.5 documentation - Google Chrome",
    "https://docs.celeryq.dev/en/stable/userguide/periodic-tasks.html",
)
TWILIO_DOCS = (
    "Google-chrome",
    "Error and Warning Dictionary | Twilio - Google Chrome",
    "https://www.twilio.com/docs/api/errors",
)
GHA_DOCS = (
    "Google-chrome",
    "Workflow syntax for GitHub Actions - GitHub Docs - Google Chrome",
    "https://docs.github.com/en/actions/writing-workflows/workflow-syntax-for-github-actions",
)
HEROKU_NW = (
    "Google-chrome",
    "northwind-memberships · Metrics | Heroku - Google Chrome",
    "https://dashboard.heroku.com/apps/northwind-memberships/metrics/web",
)
HEROKU_NW_LOGS = (
    "Google-chrome",
    "northwind-memberships · Logs | Heroku - Google Chrome",
    "https://dashboard.heroku.com/apps/northwind-memberships/logs",
)
HEROKU_NW_QA = (
    "Google-chrome",
    "northwind-qa · Settings | Heroku - Google Chrome",
    "https://dashboard.heroku.com/apps/northwind-qa/settings",
)
RDS = (
    "Google-chrome",
    "mailerdb-staging - Database details | Aurora and RDS - Google Chrome",
    "https://console.aws.amazon.com/rds/home#database:id=mailerdb-staging",
)
HN = (
    "Google-chrome",
    "Hacker News - Google Chrome",
    "https://news.ycombinator.com/",
)
HN_POST = (
    "Google-chrome",
    "Why we moved our cron jobs back into the app | Hacker News - Google Chrome",
    "https://news.ycombinator.com/item?id=45191114",
)
GMAIL = (
    "Google-chrome",
    "Inbox (3) - sam@acme.com - Gmail - Google Chrome",
    "https://mail.google.com/mail/u/0/#inbox",
)
CAL = (
    "Google-chrome",
    "Acme Engineering - Calendar - Week of 7 Sep 2026 - Google Chrome",
    "https://calendar.google.com/calendar/u/0/r/week/2026/9/7",
)
MEET_STANDUP = (
    "Google-chrome",
    "Platform standup - Google Meet - Google Chrome",
    "https://meet.google.com/kqz-pltf-std",
)
MEET_PLANNING = (
    "Google-chrome",
    "Sprint 41 planning - Google Meet - Google Chrome",
    "https://meet.google.com/abc-sprn-pln",
)
MEET_ONE = (
    "Google-chrome",
    "Sam / Jordan 1:1 - Google Meet - Google Chrome",
    "https://meet.google.com/one-sjrd-one",
)
MEET_DESIGN = (
    "Google-chrome",
    "Renewal emails: design review - Google Meet - Google Chrome",
    "https://meet.google.com/rnw-dsgn-rvw",
)
SLACK_ENG = ("Slack", "#platform-eng - Acme - Slack", None)
SLACK_NW = ("Slack", "#northwind-alerts - Acme - Slack", None)
SLACK_DM = ("Slack", "Jordan Lee - Acme - Slack", None)
SLACK_SUPPORT = ("Slack", "#support - Acme - Slack", None)
FIGMA = ("figma", "Renewal reminder email – Figma", None)
NOTES = ("Code", "notes.md — scratch — Visual Studio Code", None)

# --- the week ---------------------------------------------------------------
# A block is (start, end, [(window, weight), ...], mean_minutes) or
# ("afk", start, end). Times are local. Weight is how often the window is in
# front inside the block; mean is the typical stretch before a switch.

WORK_11381 = [(CODE_RETRY, 5), (TERM_MAILER, 4), (CODE_RETRY_TEST, 3), (JIRA_11381, 1), (TWILIO_DOCS, 1), (SLACK_ENG, 1)]
WORK_11381_PR = [(CODE_RETRY_TEST, 3), (TERM_MAILER, 4), (PR_412, 3), (CI_MAILER, 2), (SLACK_ENG, 1)]
WORK_11382 = [(CODE_RENEWAL, 5), (TERM_CONTOSO, 3), (CODE_RENEWAL_TEST, 3), (JIRA_11382, 1), (DJANGO_DOCS, 1), (CODE_TASKS, 2)]
WORK_11382_TPL = [(CODE_RENEWAL_TPL, 5), (FIGMA, 2), (DJANGO_TPL, 2), (TERM_CONTOSO, 2), (CODE_RENEWAL, 2)]
WORK_11382_PR = [(PR_415, 3), (CI_CONTOSO, 2), (TERM_CONTOSO, 3), (CODE_RENEWAL_TEST, 3), (SLACK_ENG, 1)]
WORK_11390 = [(HEROKU_NW, 3), (HEROKU_NW_LOGS, 4), (TERM_MAILER, 3), (CODE_TASKS, 3), (RDS, 2), (SLACK_NW, 2), (JIRA_11390, 1), (CELERY_DOCS, 1)]
WORK_11390_FIX = [(CODE_TASKS, 4), (TERM_CONTOSO, 3), (PR_418, 3), (HEROKU_NW_QA, 2), (SLACK_NW, 1)]
WORK_11374 = [(CODE_DEPLOY, 5), (GHA_DOCS, 2), (TERM_CONTOSO, 3), (CI_CONTOSO, 3), (JIRA_11374, 1)]
REVIEW_MON = [(PR_409, 4), (PR_410, 3), (TERM_CONTOSO, 1)]
REVIEW_TUE = [(PR_410, 3), (PR_416, 4), (TERM_CONTOSO, 1)]
REVIEW_WED = [(PR_416, 4), (PR_409, 2), (TERM_CONTOSO, 1)]
MORNING = [(SLACK_ENG, 3), (GMAIL, 2), (CAL, 1), (JIRA_BOARD, 2)]
EVENING = [(SLACK_ENG, 3), (GMAIL, 1), (JIRA_BOARD, 1), (NOTES, 2)]
LOOSE = [(HN, 2), (HN_POST, 3), (TERM_HOME, 2), (SLACK_DM, 1)]

WEEK = {
    "mon": [
        ("08:32", "08:52", MORNING, 4),
        ("08:52", "09:04", [(MEET_STANDUP, 1)], 20),
        ("09:04", "10:41", WORK_11381, 6),
        ("afk", "10:41", "10:49"),
        ("10:49", "12:12", WORK_11381, 6),
        ("afk", "12:12", "12:56"),
        ("12:56", "13:31", REVIEW_MON, 7),
        ("13:31", "14:58", WORK_11374, 6),
        ("14:58", "15:34", [(MEET_PLANNING, 6), (JIRA_BOARD, 1)], 12),
        ("15:34", "17:14", WORK_11381_PR, 6),
        ("17:14", "17:29", EVENING, 4),
    ],
    "tue": [
        ("08:36", "08:58", MORNING, 4),
        ("08:58", "09:11", [(MEET_STANDUP, 1)], 20),
        ("09:11", "10:27", WORK_11381_PR, 6),
        ("10:27", "12:04", WORK_11382, 6),
        ("afk", "12:04", "12:49"),
        ("12:49", "13:21", REVIEW_TUE, 7),
        ("13:21", "15:38", WORK_11382, 7),
        ("15:38", "16:09", [(MEET_ONE, 5), (NOTES, 1)], 12),
        ("16:09", "17:22", WORK_11382, 6),
        ("17:22", "17:33", EVENING, 4),
    ],
    "wed": [
        ("08:41", "09:01", [(SLACK_NW, 4), (HEROKU_NW, 2), (GMAIL, 1)], 4),
        ("09:01", "09:12", [(MEET_STANDUP, 1)], 20),
        ("09:12", "11:52", WORK_11390, 5),
        ("afk", "11:52", "12:40"),
        ("12:40", "14:31", WORK_11390_FIX, 6),
        ("14:31", "15:02", REVIEW_WED, 7),
        ("15:02", "17:28", WORK_11382, 7),
        ("17:28", "17:36", EVENING, 4),
    ],
    "thu": [
        ("08:30", "08:56", [(SLACK_ENG, 3), (HN, 2), (GMAIL, 1)], 5),
        ("08:56", "09:08", [(MEET_STANDUP, 1)], 20),
        ("09:08", "10:46", WORK_11382_TPL, 7),
        ("afk", "10:46", "10:53"),
        ("10:53", "12:14", WORK_11382, 6),
        ("afk", "12:14", "13:01"),
        ("13:01", "13:42", WORK_11390_FIX, 6),
        ("13:42", "14:11", REVIEW_WED, 7),
        ("14:11", "14:58", [(MEET_DESIGN, 6), (FIGMA, 2), (NOTES, 1)], 10),
        ("14:58", "17:06", WORK_11382_PR, 6),
        ("17:06", "17:24", EVENING, 4),
    ],
    "fri": [
        ("08:35", "08:57", MORNING, 4),
        ("08:57", "09:09", [(MEET_STANDUP, 1)], 20),
        ("09:09", "10:31", WORK_11382_PR, 6),
        ("10:31", "11:16", WORK_11374, 6),
        ("11:16", "11:42", LOOSE, 6),
        ("11:42", "12:41", WORK_11382, 6),
    ],
}

DATES = {
    "mon": "2026-09-07",
    "tue": "2026-09-08",
    "wed": "2026-09-09",
    "thu": "2026-09-10",
    "fri": "2026-09-11",
}


def at(day, hhmm):
    h, m = hhmm.split(":")
    return datetime.fromisoformat(f"{DATES[day]}T{h}:{m}:00").replace(tzinfo=TZ)


def stamp(t):
    return t.astimezone(ZoneInfo("UTC")).strftime("%Y-%m-%dT%H:%M:%S.") + f"{t.microsecond // 1000:03d}Z"


def emit(events, t, kind, **fields):
    row = {"ts": stamp(t), "kind": kind}
    row.update(fields)
    events.append(row)


def generate(day):
    rng = random.Random(f"site-week-{day}")
    events = []
    last = None
    first = True
    for block in WEEK[day]:
        if block[0] == "afk":
            _, start, end = block
            emit(events, at(day, start), "afk", idle=True)
            emit(events, at(day, end), "afk", idle=False)
            last = None
            continue
        start, end, windows, mean = block
        t = at(day, start) + timedelta(seconds=rng.randint(0, 40))
        stop = at(day, end)
        if first:
            emit(events, at(day, start), "afk", idle=False)
            first = False
        pool = [w for w, weight in windows for _ in range(weight)]
        while t < stop:
            window = rng.choice(pool)
            if len(windows) > 1:
                while window == last:
                    window = rng.choice(pool)
            app, title, url = window
            emit(events, t, "focus", app=app, title=title)
            if url:
                emit(events, t + timedelta(seconds=2, milliseconds=500), "url",
                     app="browser:chrome_workstation", title=title.removesuffix(" - Google Chrome"), url=url)
            last = window
            minutes = min(max(rng.expovariate(1 / mean), 0.6), mean * 3.5)
            t += timedelta(seconds=int(minutes * 60) + rng.randint(0, 59))
    # The day ends with the screen locking, the way a capture does.
    emit(events, at(day, WEEK[day][-1][2 if WEEK[day][-1][0] == "afk" else 1]), "afk", idle=True)
    return events


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for day in WEEK:
        events = generate(day)
        path = OUT / f"{day}.jsonl"
        with path.open("w") as f:
            for row in events:
                f.write(json.dumps(row, ensure_ascii=False) + "\n")
        print(f"{path.relative_to(OUT.parent.parent)}: {len(events)} events")


if __name__ == "__main__":
    main()

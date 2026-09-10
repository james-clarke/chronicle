-- m37 chunk 2 and 3: what a repo's link files say it is, and the
-- workspaces the editors remember. `repo_links` is refreshed on the git
-- poll from `.vercel/project.json`, `fly.toml`, `.sentryclirc`, … (names
-- and ids only, never contents); `url_pattern` is a host/path glob the
-- project matcher files a dashboard page into the repo's project by.
-- `kind = 'build'` rows name the build system for the standup's
-- vocabulary. `editor_workspaces` is what VS Code's recent list, JetBrains'
-- recentProjects.xml and Zed's db say was opened, local paths and remote
-- URIs, for discovery and for resolving a bare folder name in an editor
-- title to its path.
CREATE TABLE repo_links (
    repo TEXT NOT NULL,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    url_pattern TEXT,
    seen_ts INTEGER NOT NULL,
    PRIMARY KEY (repo, kind, name)
);
CREATE TABLE editor_workspaces (
    editor TEXT NOT NULL,
    path TEXT NOT NULL,
    remote TEXT NOT NULL DEFAULT '',
    last_ts INTEGER,
    seen_ts INTEGER NOT NULL,
    PRIMARY KEY (editor, path, remote)
);

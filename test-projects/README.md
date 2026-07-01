# Test projects

Simple, self-contained sample projects used to exercise QuickRunner (`qr`)
end-to-end in a dev environment. Each is a real project with a language marker
(`Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`) so `qr scan`,
`qr go`, and `qr learn` all detect it, and each solves a LeetCode-easy problem
that runs with no third-party dependencies.

| Project | Language | Problem | Run |
|---|---|---|---|
| `two-sum` | Rust | Two Sum | `cargo run` |
| `fizz-buzz` | Node | Fizz Buzz | `npm start` (or `node index.js`) |
| `palindrome-number` | Python | Palindrome Number | `python3 main.py` |
| `reverse-string` | Go | Reverse String | `go run .` |

## Using them with `qr`

Point QuickRunner at this directory and drive the core flow:

```bash
export QR_PROJECT_ROOTS="$(pwd)/test-projects"   # from the repo root
qr scan                       # discovers the 4 projects
qr go --print-path fizz       # fuzzy-jump to fizz-buzz
qr run "npm start"            # run a script (from inside a project dir)
qr learn                      # write ./.qr/profile.json for the current project
qr stats                      # aggregated run stats (enable with QR_STATS_ENABLED=true)
```

Each project also runs standalone (`cargo run`, `npm start`, `python3 main.py`,
`go run .`) and self-checks its LeetCode solution against a couple of cases.

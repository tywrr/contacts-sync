# contacts-sync

Make contacts edited in vCard 4 or JMAP clients show up properly in **Apple Contacts**, over CardDAV.

Apple Contacts (iPhone, iPad, Mac) still speaks vCard 3.0 plus Apple's own extensions. Modern clients, such as JMAP webmails (for example Bulwark on a [Stalwart](https://stalw.art) server), Thunderbird or DAVx5, write vCard 4. Both can share one CardDAV address book, but three things don't carry over:

| You set this in a vCard 4 / JMAP client | Apple Contacts expects | contacts-sync writes |
|---|---|---|
| Categories (`CATEGORIES`) | Group cards (`X-ADDRESSBOOKSERVER-KIND:group`) | One Apple group per category, with exactly its members |
| Relationships, as `@spouse Jane Doe` lines in the contact's Notes | Related names (`itemN.X-ABRELATEDNAMES` + `X-ABLabel`) | Apple related names with built-in labels where Apple has one, plus RFC 6350 `RELATED` |
| Social profiles (`SOCIALPROFILE`) | `X-SOCIALPROFILE` | A mirrored `X-SOCIALPROFILE` per profile |

It is **not** a sync between two services (for example to iCloud or Google). It works on **one** address book on your CardDAV server, which your Apple devices and your other clients both use. For a two-way sync between services, see [vdirsyncer](https://github.com/pimutils/vdirsyncer).

The vCard 4 side is the source of truth: if you edit the same groups, related names or social profiles on an Apple device, the next run overwrites them. `--migrate-relationships` does a one-time copy the other way, from existing Apple related names into `@` lines, for cards that have none.

Why `@` lines in Notes? Some JMAP clients have no relationship editor yet; Notes are editable everywhere. Lines look like `@spouse Jane Doe`, `@mother Mary Example` or `@godfather Alex Example`. Apple's built-in labels are used for spouse, partner, father, mother, parent, brother, sister, child, friend, assistant and manager; anything else becomes a custom label.

## Safety

- **Dry run by default.** Without `--apply` it prints the plan and writes nothing.
- **Conditional writes.** Every update and delete sends `If-Match` with the card's ETag; new groups use `If-None-Match: *`. A card someone changed mid-run is not overwritten.
- **Caps.** If a run would delete more than `--max-deletes` groups (default 3) or rewrite more than `--max-writes` cards (default 40), nothing is written and it exits 1. Groups are never deleted when no categories were read.
- **Everything else is preserved.** Card lines the tool doesn't manage are copied through unchanged.
- Contacts without a `UID`, and vCard 4 `KIND:group` cards, are left alone.

## Usage

```bash
export DAV_USER=you@example.com
export DAV_PASSWORD=...           # use an app password, not your main password

contacts-sync --url https://dav.example.com            # dry run: print the plan
contacts-sync --url https://dav.example.com --apply    # write the changes
```

| Flag | Env | Default | Meaning |
|---|---|---|---|
| `--url` (alias `--base`) | `CARDDAV_URL` (also reads `STALWART_URL`) | (required) | Server base URL |
| `--book-path` | | `/dav/card/{user}/{book}/` | Address book path; `{user}` is percent-encoded |
| `--book` | | `default` | Address book name |
| `--apply` | | off | Write changes |
| `--migrate-relationships` | | off | One-time: Apple related names into Notes `@` lines |
| `--max-deletes` | | 3 | Most groups deleted per run |
| `--max-writes` | | 40 | Most cards rewritten per run |

The default book path is Stalwart's layout. Other servers use different paths, for example `/remote.php/dav/addressbooks/users/{user}/{book}/` on Nextcloud or `/{user}/{book}/` on Radicale. Only Stalwart has been tested.

Output is one summary line, one line per planned change, then either `dry run: nothing written`, `blocked: nothing written`, or the HTTP status of each write.

**Exit codes:** `0` on a dry run or when every write succeeded; `1` on any error, when a cap is exceeded (nothing written), or when any write failed (the remaining writes still run).

### Container

The image is a static binary on an empty base (about 2 MB), running as an unprivileged user:

```bash
docker run --rm -e DAV_USER -e DAV_PASSWORD ghcr.io/tywrr/contacts-sync --url https://dav.example.com
```

### Kubernetes CronJob

```yaml
apiVersion: batch/v1
kind: CronJob
metadata:
  name: contacts-sync
spec:
  schedule: "*/15 * * * *"
  concurrencyPolicy: Forbid
  jobTemplate:
    spec:
      activeDeadlineSeconds: 300
      template:
        spec:
          restartPolicy: Never
          securityContext:
            runAsNonRoot: true
            runAsUser: 65534
          containers:
            - name: contacts-sync
              image: ghcr.io/tywrr/contacts-sync:latest   # pin a digest in production
              args: ["--apply", "--url", "https://dav.example.com"]
              envFrom:
                - secretRef:
                    name: contacts-sync   # DAV_USER and DAV_PASSWORD
              securityContext:
                allowPrivilegeEscalation: false
                readOnlyRootFilesystem: true
                capabilities:
                  drop: ["ALL"]
```

## How it works

1. One CardDAV `REPORT addressbook-query` (Depth 1) reads every card and its ETag.
2. Categories are collected across contacts; Apple group cards are matched by name, and new ones get a stable UUIDv5 from the category name.
3. Each contact is checked for `@` lines and social profiles, and rewritten only if the Apple-side lines differ. Lines are folded at 75 octets.
4. Writes go out with `If-Match`, after the caps are checked.

There's no incremental sync token: the whole book is read each run, which is fine for personal address books (tested on about a thousand cards).

## Development

```bash
cargo test           # 14 unit tests: multistatus parsing, vCard helpers, planning and rewrites
cargo clippy --all-targets
cargo build --release
```

The Docker build runs the tests too, so a failing test fails the image.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.

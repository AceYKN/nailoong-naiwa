# Security and privacy

NLNF Classifier is local-first. The following rules are part of the product
boundary, not optional deployment advice:

- QQ cache images, chat content, labels, SQLite databases, descriptors, and logs
  stay local by default. Do not commit them or upload them to issue trackers.
- The v2 runtime has no cloud prelabeling, training service, or image-upload
  path. Never place an API key in source code, arguments, logs, or this
  repository. If a key is ever pasted into chat, a terminal transcript, or a
  repository, revoke and replace it immediately.
- The OneBot API and reverse-event boundary bind to localhost/loopback only
  and bound request/image sizes.
- QQ side effects are disabled by default. `OBSERVE` records candidates without
  recall; `AUTO_RECALL` requires an explicitly enabled group and a separately
  verified precision gate. A preview build or a unit test is not production
  authorization.

## Before publishing a change

Run the repository tests and check that no private artifacts are staged:

```powershell
git status --short
git diff --cached --check
rg -n --hidden --glob '!node_modules/**' --glob '!apps/desktop/src-tauri/target/**' --glob '!.git/**' "sk-[A-Za-z0-9]{20,}" .
```

If you discover a security issue, do not include QQ images, chat contents,
tokens, or other private data in a public issue. Contact the repository owner
privately with a minimal reproduction and the affected commit or version.

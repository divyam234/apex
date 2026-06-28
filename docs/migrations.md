# Data migrations

ApexAPI workspace documents are versioned and human-readable. Loaders preserve unknown fields where supported and writes use atomic replacement. History storage migrates through schema version 2, adding optional bounded request/response snapshots while retaining metadata-only defaults.

Before upgrading, commit or back up the workspace and `.apex` state. After upgrading, run `apex workspace validate` where available and inspect Git changes before committing. Downgrades are not guaranteed to understand fields introduced by newer versions; use a branch or backup for rollback.

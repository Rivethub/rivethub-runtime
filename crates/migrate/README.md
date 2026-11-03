# migrate

Every Postgres and SQLite schema change. `rivethub-migrate postgres|sqlite` (in the `cli` crate) applies them; the
runtime only verifies the version. Postgres migrations are `migrations/NNNN_name.sql`, run in order by
`sqlx::migrate!`; each also records its file name in `_rivetos_migrations`. SQLite builds schema v7 and upgrades it step
by step (`src/schema/`), copying the file to `<file>.v<N>.bak` before any read-write connection.

Migration 0052 creates the job queue on a database that has none: the `graphile_worker` schema of
[graphile-worker](https://github.com/graphile/worker) 0.17.3, reproduced from what its `--schema-only` install creates
(CONTRACT note 105). That schema is graphile-worker's work, used under its licence:

> The MIT License (MIT)
>
> Copyright © 2019 Benjie Gillam
>
> Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated
> documentation files (the “Software”), to deal in the Software without restriction, including without limitation the
> rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit
> persons to whom the Software is furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all copies or substantial portions of the
> Software.
>
> THE SOFTWARE IS PROVIDED “AS IS”, WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE
> WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
> COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
> OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

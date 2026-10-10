# Research kit (cercis-kits-research)

**English** | [中文](README.zh-CN.md)

The research-specific plugins for Cercis. **It is a separate repository from the basic kit**
(`cercis-kits-basic`): the two do not depend on each other, can be installed and evolved
separately, and both work only through the kernel command line with `--json`.

> **Distribution**: this kit is **not part of the app installer** (the installer contains the
> kernel + the basic kit only). The repository ships a `marketplace.json` that the app uses as a
> **preconfigured plugin marketplace** — you install it with a click inside the app, and it is
> **not downloaded by default** (D35 / FR-P44–P50).

## How it splits with the basic kit

| | Basic kit | Research kit (this repository) |
|---|---|---|
| For | any domain (research, business ledgers, inventory, equipment logs, …) | research settings |
| Content | project wizard, data entry, import/export, health check, plugin manager | research templates, terminology packs, discipline checks, xlsx/docx and scanning flows, SPSS/R export, print & QR labels, history review, NAS, to-dos |
| Repository | `cercis-kits-basic` | `cercis-kits-research` |

## Status

**The research kit is in place (27 tests)**: four plugins — templates & terminology,
import/export (xlsx / SPSS / R), printing & QR labels, and the field scribe (voice + handwriting
with per-cell confidence). Layout:

```
research/
├── templates/field-trial.yaml   field-trial template (9 tables, references and ranges declared)
├── terms/                      terminology packs: generic / agriculture / medicine
├── crates/
│   ├── plugin-research-template/  plugin 1 · templates & terminology
│   ├── plugin-research-io/        plugin 2 · import & export (xlsx / codebook / SPSS / R)
│   ├── plugin-research-print/     plugin 3 · printable record sheets and QR labels
│   └── plugin-research-scribe/    plugin 4 · field scribe (voice + handwriting → draft)
├── plugins/                    manifests (plugin.yaml) and notes for each plugin
└── marketplace.json            this repository as the app's preconfigured marketplace
```

```bash
# build and test (the kernel must be built first)
cargo build && cargo test     # includes cross-process end-to-end: install template → record data
                              # → switch terminology pack → data bytes unchanged

# try it by hand
export CERCIS_BIN=<path to kernel> CERCIS_RESEARCH_DIR=$PWD
./target/debug/cercis-plugin-research-template list
./target/debug/cercis-plugin-research-template install --project <project> --template field-trial --terms agriculture --allow-write
```

What has been completed (originally S7–S9):

| Step | Content | Acceptance |
|------|---------|-----------|
| S7 ✅ | research templates, terminology packs, method documents | after loading the research template the UI shows study/plot/sample/observation fields; switching terminology does not touch the data (verified byte by byte) |
| S8 ✅ | xlsx import wizard, codebook, SPSS / R export (with value coding and labels) | record sheets import cleanly, the whole batch is validated, export notes are traceable |
| S9 ✅ | printable record sheets and QR labels, field scribe (voice + handwriting + per-cell confidence + review before commit) | the whole "generate sheet → write by hand → scan → check → commit" loop works; the app can replay audio, zoom into photos and confirm cell by cell |

> PDF export needs Chrome or Edge on the machine (otherwise HTML is produced; set
> `CERCIS_BROWSER` to point at a specific browser).
> NAS and working copies are **kernel** capabilities (`cercis remote` / `cercis workcopy`) and are
> not reimplemented here.

## Planned plugin shape

Same as the basic kit: **a set of independent plugins**, each with a `plugin.yaml` and an
executable entry point, individually enabled and authorised; writes always go through the kernel
and are read-only by default.

```
research/
├── crates/          core capability of each plugin (Rust, platform-neutral)
└── plugins/         manifests and notes shipped with the kit
```

## License

Apache-2.0 (see `LICENSE` and `NOTICE`).

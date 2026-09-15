# Bundled Reference Assets

The desktop build contains the seven images that were selected as
`positive_reference` in the local review package on 2026-09-15:

| Class | Review ID | Bundled file |
| --- | --- | --- |
| `NAIWA_FROG` | `NF-OFF-001` | `NF-OFF-001-naiwa-hero.png` |
| `NAIWA_FROG` | `NF-OFF-002` | `NF-OFF-002-naiwa-belly.png` |
| `NAIWA_FROG` | `NF-OFF-003` | `NF-OFF-003-naiwa-lying.png` |
| `NAIWA_FROG` | `NF-OFF-004` | `NF-OFF-004-naiwa-rolling.png` |
| `NAIWA_FROG` | `NF-OFF-005` | `NF-OFF-005-naiwa-wave.png` |
| `NAIWA_FROG` | `NF-OFF-006` | `NF-OFF-006-naiwa-logo.png` |
| `NAILONG` | `NL-OFF-011` | `NL-OFF-011-nailong-brand-example-10.png` |

The candidates came from the publicly accessible official pages
[milkyfrog.com](https://milkyfrog.com/) and
[nailoong.com](https://www.nailoong.com/). The review package recorded the
corresponding source asset URLs and SHA-256 values before selection. The
remaining candidates were deliberately kept as test-only or excluded and are
not part of this repository.

These files are reference prototypes for the zero-training local matcher,
not a training dataset. On first Windows startup they are copied into the
user's app-data Reference Bank; they are not uploaded by the application.

Public page availability does not by itself grant a redistribution license.
The repository keeps this source note so maintainers can verify the applicable
rights before redistributing a release or replacing the default pack. The
`NF-OFF-006` logo/文字 image is included because it was explicitly selected in
the user review, although clean pose references are preferred for matching.

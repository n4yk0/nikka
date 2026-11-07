# Portage de nikka en Rust

Branche : `feature/rust-port`. La bascule est faite sur cette branche : le code Go en est supprimé. `main` garde la version Go jusqu'au merge.

## Objectif

Remplacer l'implémentation Go par une implémentation Rust à parité fonctionnelle :

- même CLI : commandes `all`, `sys`, `clean`, `intel`, `news`, `skills`, `jobs`, `init`, `version` et flags `-y/--yes`, `-n/--dry-run`, `--check`, `--no-anim`, `--no-links`, `--config` ;
- même fichier de configuration `~/.config/nikka/config.toml`, lu et écrit dans le même format ;
- mêmes codes de sortie : 0 succès (avertissements compris), 1 échec, 2 erreur d'usage ou de configuration ;
- même rendu terminal : logo pixelisé animé, bannière, sections, rapport final.

## Stratégie

- Le crate Rust a été construit à la racine du dépôt (`Cargo.toml`, `src/`, `examples/`), à côté du code Go, qui est resté intact et testé jusqu'à la bascule.
- Portage module par module, en commençant par les feuilles. Chaque module est arrivé avec ses tests, portés depuis les tests Go puis complétés.
- Parité vérifiée en comparant la sortie des deux binaires sur la machine (voir [Comparer Go et Rust](#comparer-go-et-rust)).
- Pendant la migration, le binaire Rust s'installait sous le nom `nikka-rs` pour cohabiter avec `nikka` (Go). Ces cibles ont disparu à la bascule ; un `nikka-rs` installé se retire avec `rm -f ~/.local/bin/nikka-rs`.

## État

| Paquet Go | Module Rust | État | Remarques |
|---|---|---|---|
| `cmd/nikka` | `src/main.rs` | porté | parsing par clap |
| `internal/config` | `src/config.rs` | porté | lit un fichier écrit par la version Go (test dédié) |
| `internal/execx` | `src/execx/` | porté | ajout du trait `RootAccess` pour tester sans sudo |
| `internal/textutil` | `src/textutil.rs` | porté | |
| `internal/logo` + `logogen` | `src/logo/`, `examples/logogen.rs` | porté | grille régénérée depuis le PNG identique à celle de Go (test) |
| `internal/ui` | `src/ui/` | porté | styles, police, spinner, bannière, saisie interruptible |
| `internal/system` | `src/system.rs` | porté | apt, snap, flatpak, fwupd, reboot-required |
| `internal/cleanup` | `src/cleanup/` | porté | docker, /tmp, tmpfiles, apt, journal, snap, flatpak, miniatures, corbeille |
| `internal/routine` (sys, clean, bannière, rapport) | `src/routine/` | porté | |
| `internal/httpx` | `src/httpx/` | porté | cache partagé avec la version Go (même nommage, même JSON) |
| `internal/news` | `src/news/` | porté | lecteur RSS/Atom maison (voir plus bas) |
| `internal/routine/intel.go` (news, lancements, KEV) | `src/routine/intel.rs` | porté | préchargement en début d'exécution |
| `internal/skills` | `src/skills/` | porté | taxonomie, analyse, fils HN « Who is hiring? » |
| `internal/jobs` | `src/jobs/` | porté | France Travail, Adzuna, OSRM, score ; trait `Board` commun aux deux job boards |
| `internal/routine/intel.go` (skills, jobs, liens rapides) | `src/routine/market.rs` | porté | |

Tous les modules sont portés. Phase 5 (bascule) faite : `cmd/`, `internal/`, `go.mod` et `go.sum` supprimés, logo déplacé dans `assets/logo.png`. Les cibles `make build`, `test`, `lint`, `generate` et `install` passent par cargo ; `make install` installe `~/.local/bin/nikka`.

Vérifications sur la machine le 16/10/2026 :

- `nikka sys -n --no-anim` et `nikka clean -n --no-anim` produisent une sortie identique en Go et en Rust (horloge, uptime et durées exclus du diff), bannière comprise ;
- `nikka news --no-anim` lancé avec les deux versions coup sur coup sur les flux réels : sortie identique ligne à ligne (25 articles, lancements SpaceX, CVE du catalogue KEV), comptes à rebours exclus du diff ;
- une entrée de cache écrite par la version Rust est relue par la version Go, et inversement ;
- `nikka skills`, `nikka jobs` et `nikka -n` (les cinq sections) lancés avec les deux versions : sortie identique ligne à ligne, sans identifiants de job boards (skills sur les seuls fils HN, jobs en mode « no job board configured »).

France Travail et Adzuna n'ont été vérifiés qu'avec des serveurs de test qui reproduisent leurs réponses documentées (jeton OAuth, pagination `Content-Range`, nouvelle tentative sur HTTP 429, coordonnées en nombre ou en chaîne, clé d'API absente des erreurs). Faute d'identifiants, aucun appel réel n'a été fait, ni en Go ni en Rust : à valider dès que les clés sont disponibles.

## Dépendances

| Besoin | Go | Rust | Version | Documentation |
|---|---|---|---|---|
| CLI | `flag` | clap (derive) | 4.6 | https://docs.rs/clap |
| TOML | BurntSushi/toml | toml + serde | 1.1 / 1.0 | https://docs.rs/toml |
| JSON | `encoding/json` | serde_json | 1.0 | https://docs.rs/serde_json |
| Expressions régulières | `regexp` (RE2) | regex (syntaxe RE2, sans lookaround) | 1.13 | https://docs.rs/regex |
| Entités HTML | `html.UnescapeString` | htmlize, feature `unescape` (liste WHATWG, entités sans `;` comprises) | 1.1 | https://docs.rs/htmlize |
| Suppression des accents | `golang.org/x/text` | unicode-normalization | 0.1 | https://docs.rs/unicode-normalization |
| Largeur d'affichage | `x/ansi` | unicode-width + `src/ui/ansi.rs` | 0.2 | https://docs.rs/unicode-width |
| Couleurs, dégradé | lipgloss, go-colorful | `src/ui/theme.rs`, `src/ui/font.rs` (interpolation L\*u\*v\*) | — | — |
| Terminal, uid, statvfs, poll | `x/term`, `syscall` | rustix | 1.1 | https://docs.rs/rustix |
| Dates et fuseaux | `time` | jiff (`%Z` donne `CEST`) | 0.2 | https://docs.rs/jiff |
| Signaux | `os/signal` | signal-hook | 0.4 | https://docs.rs/signal-hook |
| HTTP | `net/http` | ureq (synchrone, rustls, gzip) | 3.4 | https://docs.rs/ureq |
| RSS/Atom | gofeed | `src/news/feed.rs` sur quick-xml | 0.42 | https://docs.rs/quick-xml |
| Clé de cache | `crypto/sha256` | sha2 | 0.11 | https://docs.rs/sha2 |
| PNG (logogen, dev) | `image/png` | png | 0.18 | https://docs.rs/png |
| Répertoires temporaires (tests) | `t.TempDir` | tempfile | 3 | https://docs.rs/tempfile |

Le cache HTTP nomme ses fichiers avec les 12 premiers octets du SHA-256 de l'URL en hexadécimal, comme la version Go ; les deux versions partagent donc `~/.cache/nikka/`.

feed-rs a été écarté : il ne restitue pas les éléments non standard d'un item RSS, alors que la source Le Temps est filtrée sur `<free>true</free>` (gofeed les expose dans `Custom`). Le lecteur maison couvre RSS 2.0, RSS 1.0 (RDF) et Atom, tolère les entités HTML non déclarées (`&nbsp;`) et les balises mal fermées, et lit les dates RFC 2822, RFC 3339 et leurs variantes courantes, jour de semaine erroné compris.

## Toolchain

`rust-version = "1.87"`, la version installée. En édition 2024, Cargo utilise le resolver `"3"`, qui préfère les versions de dépendances compatibles avec `rust-version` ([documentation Cargo, section Rust version](https://doc.rust-lang.org/cargo/reference/resolver.html#rust-version)). Exemple : sha2 0.11.1 exige Rust 1.88, le resolver a donc retenu la 0.11.0 (Rust 1.85). Un `rustup update` lèverait ces contraintes ; il faudrait alors relever `rust-version`.

## Écarts assumés avec la version Go

- Aide et messages d'usage générés par clap : présentation différente, `-V/--version` en plus, plus de flags longs à un seul tiret (`-dry-run`).
- Couleurs en 24 bits uniquement, comme le logo l'était déjà. lipgloss réduisait à 256 ou 16 couleurs sur les terminaux sans truecolor.
- Un processus enfant tué par un signal est rapporté `killed by SIGINT` au lieu de `exit status -1`.
- Si `apt-get -s autoremove` échoue pendant l'estimation, la taille du cache apt s'affiche `—` au lieu de la taille connue.
- L'opérateur de la bannière vient de `$USER` (repli sur `$LOGNAME`) au lieu d'une recherche dans `/etc/passwd`.
- La suppression des accents retire toutes les marques combinantes (Mn, Mc, Me) et pas seulement Mn ; aucune différence sur du texte latin.
- Les flux sont lus en UTF-8 ; gofeed convertissait les autres encodages déclarés. Tous les flux par défaut sont en UTF-8 (vérifié le 16/10/2026), un flux Latin-1 ajouté en configuration aurait ses accents altérés.
- Les échecs de sources de news et de job boards s'affichent dans l'ordre de la configuration, et non dans l'ordre d'arrivée des réponses.
- Un libellé de salaire France Travail non reconnu est tronqué à 40 caractères et non à 40 octets (Go pouvait couper un caractère accentué en deux).
- Un lancement sans date (`net` absent) s'affiche `NET TBD`.

## Interruption (Ctrl-C)

SIGINT et SIGTERM sont interceptés pendant toute l'exécution, comme en Go :

- une commande en cours n'est jamais tuée par nikka ; le SIGINT du terminal lui parvient directement et nikka continue de lire sa sortie, si bien qu'apt ou dpkg ne meurent pas sur un tube fermé ;
- les étapes restantes sont sautées, le rapport final indique « interrupted by operator » et le code de sortie vaut 1 ;
- une question en attente (`[Y/n]`) est abandonnée immédiatement et vaut « non » ;
- les téléchargements déjà lancés (préchargement des flux) ne sont pas annulés : leur délai maximal est de 20 secondes, la section qui les attend est sautée.

La version Go a un défaut sur ce dernier point : après Ctrl-C, la question continue d'attendre et un simple Entrée prend la réponse par défaut, ce qui peut lancer les mises à jour si sudo a déjà été obtenu pour le rafraîchissement des index. La version Rust le corrige ; `main` reste concerné jusqu'au merge.

## Commandes

```sh
make build          # cargo build --release --locked
make test           # tests unitaires, tests de logogen compris
make lint           # cargo fmt --check + clippy -D warnings
make generate       # régénère src/logo/grid.txt depuis assets/logo.png
make install        # ~/.local/bin/nikka
make uninstall

cargo run -- sys -n --no-anim
```

## Comparer Go et Rust

Uniquement en `--dry-run` pour `sys` et `clean` : ni root, ni modification. `news`, `skills` et `jobs` ne modifient rien ; lancer les deux versions coup sur coup pour comparer les mêmes données. La version Go n'existe plus que sur `main` : la sortir dans un worktree à côté.

```sh
git worktree add ../nikka-go main
norm() { sed -E 's/clock .*/clock X/;s/uptime .*/uptime X/;s/[0-9]+(\.[0-9]+)?(ms|s)$/T/;s/completed in .*/completed in X/;s/T[-+][0-9d ]*[0-9]{2}:[0-9]{2}:[0-9]{2}/T-X/'; }
for cmd in sys clean news skills jobs; do
  (cd ../nikka-go && go run ./cmd/nikka "$cmd" -n --no-anim) | norm > /tmp/go-$cmd.txt
  cargo run -q -- "$cmd" -n --no-anim | norm > /tmp/rs-$cmd.txt
  diff /tmp/go-$cmd.txt /tmp/rs-$cmd.txt && echo "$cmd identique"
done
git worktree remove ../nikka-go
```

## Retour arrière

- Avant le merge : `main` garde la version Go. Pour revenir au binaire Go après un `make install` depuis cette branche : `git switch main && make install` (toolchain Go requise).
- Après le merge : `git revert -m 1 <commit de merge>`, puis `make install` reconstruit la version Go.
- Dans les deux cas, aucune donnée n'est à migrer : la configuration garde le même format et le cache HTTP le même nommage, les deux versions lisent les fichiers de l'autre.

## Prochaines étapes

1. Avec des identifiants France Travail (et Adzuna en option) : lancer `nikka jobs` et `nikka skills` avec les deux versions et comparer (voir [Comparer Go et Rust](#comparer-go-et-rust)).
2. Merge de `feature/rust-port` dans `main`.
3. Sur la machine : `make install` remplace le binaire Go `~/.local/bin/nikka` par la version Rust.

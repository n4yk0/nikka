# nikka

Routine quotidienne pour le poste Linux, en une commande :

1. **SYS.PATCH** : vérification et mise à jour de tous les paquets (apt, snap, flatpak) et contrôle des firmwares (fwupd, lecture seule).
2. **SYS.PURGE** : suppression des images Docker inutilisées, du cache de build, des fichiers périmés de `/tmp`, des paquets orphelins, des vieux journaux, des révisions snap désactivées, des miniatures et de la corbeille ancienne.
3. **INTEL.FEED** : actualités les plus importantes en IA, SpaceX (avec les prochains lancements), cybersécurité (avec les dernières CVE exploitées selon la CISA), France et Suisse.
4. **MARKET.SKILLS** : top 5 des compétences demandées aux ingénieurs logiciel, mesuré sur de vraies offres, avec la tendance d'un mois sur l'autre.
5. **MARKET.JOBS** : top 5 des offres à 45 minutes de route maximum de l'origine configurée (Bordeaux par défaut).

Interface terminal style « hacker » : logo pixel art animé, couleurs truecolor, spinners, liens cliquables (OSC 8).

## Installation

Prérequis : Rust 1.87+ (cargo), Linux. Docker, snap et flatpak sont détectés automatiquement.

```sh
make install        # binaire dans ~/.local/bin/nikka
nikka init          # écrit ~/.config/nikka/config.toml (0600)
```

## Utilisation

```sh
nikka               # routine complète : patch + purge + briefing
nikka --dry-run     # affiche tout ce qui serait exécuté, ne modifie rien, ne demande jamais root
nikka --check       # rafraîchit les index et rapporte, sans upgrade ni purge
nikka intel         # news + skills + jobs, sans root
nikka sys | clean | news | skills | jobs
nikka -y            # répond oui à toutes les confirmations
nikka work          # ouvre un terminal sur 4 panneaux (aussi : nikka --work)
```

| Flag | Effet |
|---|---|
| `-y`, `--yes` | confirme automatiquement (sauf suppression de paquets par apt, toujours demandée) |
| `-n`, `--dry-run` | aucune commande privilégiée ni destructive, liste des commandes prévues |
| `--check` | index rafraîchis, rapport seul |
| `--no-anim` | désactive l'animation de démarrage |
| `--no-links` | désactive les hyperliens terminal |
| `--config PATH` | fichier de configuration alternatif |
| `--work` | équivaut à la commande `work` |
| `--terminal NAME` | terminal de `work` : `auto`, `kitty`, `ghostty`, `ptyxis`, `gnome-terminal`, `konsole`, `xfce4-terminal`, `x-terminal-emulator` |

Codes de sortie : `0` succès (avertissements compris), `1` au moins une étape en échec, `2` erreur d'usage ou de configuration.

Sans terminal interactif (cron, pipe), les confirmations répondent « non » sauf avec `--yes` ; animations, spinners et couleurs sont désactivés.

## Session de travail

`nikka work` ouvre une nouvelle fenêtre de terminal découpée en 4 panneaux (grille 2×2), tous dans le répertoire courant.

- **Terminal** : `auto` (par défaut) prend le terminal dans lequel nikka tourne (kitty, Ghostty, Konsole, GNOME Terminal), sinon celui de `$TERMINAL`, sinon le premier installé dans l'ordre kitty, Ghostty, Ptyxis, GNOME Terminal, Konsole, Xfce Terminal, `x-terminal-emulator`. `--terminal` ou `[work] terminal` imposent un choix.
- **Avec tmux** (recommandé) : nikka crée la session tmux `work` découpée en 2×2, puis le terminal s'y attache. Si la session existe déjà, elle est rattachée telle quelle, sans nouveau découpage. Lancé depuis tmux, nikka retire `$TMUX` pour que la nouvelle fenêtre puisse s'attacher.
- **Sans tmux** : nikka suggère de l'installer (`sudo apt install tmux` sur Debian/Ubuntu/Kali), puis utilise ce que le terminal sait faire :
  - kitty : grille 2×2 native, via un fichier de session écrit dans `~/.cache/nikka/` ;
  - les autres (Ghostty, Ptyxis, GNOME Terminal, Konsole, Xfce Terminal) ne savent pas créer de panneaux en ligne de commande : nikka ouvre 4 fenêtres.
- `--dry-run` affiche les commandes sans rien ouvrir. Sans session graphique (`DISPLAY` et `WAYLAND_DISPLAY` vides), la commande échoue.

## Détail des modules

### SYS.PATCH

| Gestionnaire | Vérification | Mise à jour |
|---|---|---|
| apt | `apt-get update`, puis `apt-get -s full-upgrade` (simulation) | `apt-get -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold full-upgrade` |
| snap | `snap refresh --list` | `snap refresh` |
| flatpak | `flatpak remote-ls --updates` | `flatpak update -y --noninteractive` |
| fwupd | `fwupdmgr get-updates --json` | aucune : affichage seul, à lancer manuellement avec `fwupdmgr update` |

- `sudo` n'est demandé qu'une fois, au premier besoin réel, puis maintenu actif pendant l'exécution.
- Les mises à jour différées par le *phasing* Ubuntu sont signalées, pas forcées.
- Si `full-upgrade` prévoit de **supprimer** des paquets, une confirmation explicite est exigée, même avec `--yes`.
- `--force-confold` conserve les fichiers de configuration modifiés localement.
- Un redémarrage en attente (`/var/run/reboot-required`) est seulement rappelé, avec la commande `systemctl reboot`.
  - La routine continue (purge, briefing) et ne redémarre jamais la machine.
  - Le code de sortie reste 0.
- Les commandes lancées n'ont jamais accès au clavier : leur entrée standard est `/dev/null`. Aucune invite d'après mise à jour ne peut donc bloquer, y compris l'écran « Pending kernel upgrade » de needrestart.
- Un Ctrl-C n'interrompt jamais une commande en cours : nikka continue de lire sa sortie jusqu'au bout, si bien qu'apt ou dpkg ne meurent pas sur un tube fermé, puis saute les étapes restantes (code de sortie 1). Une question en attente est abandonnée et vaut « non ».

### SYS.PURGE

Le plan affiché avant confirmation donne la taille récupérable estimée de chaque cible.

| Cible | Commande / règle | Root |
|---|---|---|
| Images Docker inutilisées | `docker image prune --all --force` | non (groupe docker) |
| Cache de build Docker | `docker builder prune --all --force` | non |
| Conteneurs arrêtés | `docker container prune --force` (désactivé par défaut) | non |
| `/tmp` | entrées appartenant à l'utilisateur, intactes depuis 7 jours | non |
| Politique tmpfiles | `systemd-tmpfiles --clean` | oui |
| apt | `apt-get autoremove --purge -y` puis `apt-get clean` | oui |
| Journal systemd | `journalctl --vacuum-time=2weeks` | oui |
| Révisions snap désactivées | `snap remove <nom> --revision=<rev>` | oui |
| Miniatures | fichiers de `~/.cache/thumbnails` de plus de 30 jours | non |
| Corbeille | éléments supprimés il y a plus de 30 jours (champ `DeletionDate`, spécification freedesktop) | non |

Garde-fous sur `/tmp` :

- seules les entrées de l'utilisateur courant sont visées ;
- un dossier n'est supprimé que si **tout** son contenu est plus vieux que le seuil ;
- un dossier qui contient une socket, un pipe, un fichier d'un autre propriétaire ou un élément illisible est conservé ;
- liste d'exclusion par préfixe : `.X*`, `systemd-private-*`, `ssh-*`, `tmux-*`, etc.

**Les volumes Docker ne sont jamais purgés** : ils peuvent contenir des bases de données. Le plan rappelle seulement la commande pour les inspecter.

### INTEL.FEED

Le classement combine quatre facteurs :

- la fraîcheur, avec une demi-vie propre à chaque catégorie ;
- la position éditoriale, pour les flux « une » ;
- la couverture : une même histoire reprise par plusieurs sources, regroupée par similarité de titres, remonte ;
- un poids par source.

Les sources par défaut ont été vérifiées le 2026-10-05 (France et Suisse le 2026-10-18) et sont modifiables dans `[[news.categories]]`. Les catégories France et Suisse ne lisent que les rubriques nationales : les unes mélangent l'actualité internationale.

| Catégorie | Sources |
|---|---|
| IA | TechCrunch AI, The Verge AI, MIT Technology Review (articles sponsorisés exclus), Google News, Hacker News (titres IA) |
| SpaceX | Spaceflight News API v4, Google News, Launch Library 2 (prochains lancements, cache 1 h, quota gratuit de 15 requêtes/h) |
| Cybersécurité | CERT-FR alertes, The Hacker News, BleepingComputer, Krebs on Security, CISA KEV (cache 6 h, requêtes conditionnelles) |
| France | Le Monde (politique, société), franceinfo (France), Google News FR (rubrique France) |
| Suisse | RTS Info (Suisse), Le Temps (Suisse, articles gratuits seulement), 24 heures (Suisse), Google News CH (rubrique Suisse) |

Les liens Google News sont des URL de redirection Google : le navigateur les résout, l'outil ne les réécrit pas.

### MARKET.SKILLS

Les chiffres viennent des offres réelles, pas d'une liste figée.

- Une taxonomie d'environ 40 compétences, sous forme d'expressions régulières sur le texte normalisé (minuscules, sans accents), est appliquée à trois corpus :
  - **HN** : toutes les annonces du dernier fil mensuel « Ask HN: Who is hiring? », marché tech mondial, via l'API Algolia ;
  - **FR** : échantillon national récent d'offres de développement, via France Travail et Adzuna (si configurés) ;
  - **local** : les offres à 45 minutes ou moins, si au moins 8 sont trouvées.
- Chaque corpus pèse le même poids dans le score combiné.
- La colonne `Δ m/m` compare le fil HN courant au fil du mois précédent, en points de pourcentage.
- Les « market signals » suivent explicitement trois tendances :
  - la part des offres qui mentionnent l'IA ;
  - le vocabulaire « product engineer » (glissement développeur vers product engineer) ;
  - le télétravail.

Le fil HN du mois courant grossit pendant les premiers jours : les deltas sont plus fiables après le 10 du mois.

### MARKET.JOBS

1. Recherche autour de l'origine configurée (par défaut Bordeaux, INSEE 33063, rayon de 60 km) :
   - France Travail : domaine `M18` Informatique / Télécoms, offres publiées depuis 31 jours, jusqu'à 450 offres ;
   - Adzuna : recherche sur le code postal de l'origine.
2. Calcul du **temps de trajet réel en voiture** avec le service `table` d'OSRM, en une requête groupée. Si OSRM est indisponible, l'outil se rabat sur une estimation à vol d'oiseau, signalée par `~`.
3. Rejet des offres à plus de 45 minutes, des alternances et stages, et des doublons.
4. Score de chaque offre :

   | Critère | Poids |
   |---|---|
   | Pertinence de l'intitulé | 35 % |
   | Correspondance avec `profile_skills` | 30 % |
   | Fraîcheur | 15 % |
   | Type de contrat (CDI > freelance > CDD > intérim) | 10 % |
   | Proximité | 10 % |

`profile_skills` est vide par défaut : renseignez vos compétences dans la configuration pour activer ce critère.

Des liens de recherche prêts à l'emploi (France Travail, LinkedIn) sont toujours affichés en fin de section.

## Identifiants des job boards

Les identifiants se placent dans la configuration ou, de préférence, dans des variables d'environnement, qui ont priorité sur le fichier.

**France Travail** (gratuit, recommandé) :

1. Créer un compte et une application sur <https://francetravail.io>.
2. Abonner l'application à l'API « Offres d'emploi v2 ».
3. Exporter les identifiants :

```sh
export FRANCE_TRAVAIL_CLIENT_ID=...
export FRANCE_TRAVAIL_CLIENT_SECRET=...
```

**Adzuna** (optionnel) : inscription sur <https://developer.adzuna.com/signup>. Le palier gratuit permet 25 requêtes par minute et 250 par jour ; une exécution en consomme au plus 5. L'attribution « Jobs by Adzuna » est affichée quand des offres Adzuna sont présentes, comme l'exigent leurs conditions.

```sh
export ADZUNA_APP_ID=...
export ADZUNA_APP_KEY=...
```

Les clés n'apparaissent jamais dans les messages d'erreur : les query strings sont retirées des URL.

## Rollback

| Action | Retour arrière |
|---|---|
| Upgrade apt | Pas de rollback automatique. Réinstaller une version précise avec `sudo apt install paquet=version` si elle est encore dans le dépôt ou dans `/var/cache/apt/archives`. **Après `apt-get clean`, le cache local est vidé.** L'historique est dans `/var/log/apt/history.log`. |
| Refresh snap | `sudo snap revert <nom>`, tant que la révision précédente existe. **La purge des révisions désactivées supprime cette possibilité.** |
| Images et cache Docker | Images à re-télécharger avec `docker pull` ou à reconstruire. |
| `/tmp`, miniatures, corbeille, journaux | **Irréversible.** Utiliser `--dry-run` pour vérifier la liste avant. |

Pour garder une possibilité de retour arrière, lancer `nikka sys` puis vérifier le système avant `nikka clean`.

## Configuration

`nikka init` génère le fichier complet avec les valeurs par défaut. Points utiles :

- `[clean]` : seuils d'âge, exclusions de `/tmp`, activation par cible Docker.
- `[news]` : nombre d'articles par catégorie, sources (remplacer `[[news.categories]]` remplace toute la liste).
- `[jobs]` :
  - origine (`origin_lat`, `origin_lon`, `origin_insee`, `origin_postal_code`) ;
  - budget `max_drive_minutes`, rayon de recherche `search_radius_km` ;
  - `profile_skills` ;
  - `routing_url`, pour pointer vers une instance OSRM auto-hébergée.
- `[work]` : `terminal` (`auto` par défaut) et `session`, le nom de la session tmux (`work` par défaut).

Le cache HTTP (lancements, KEV, fils HN) est dans `~/.cache/nikka/`. En cas de coupure réseau, la dernière copie est servie et marquée `(cached, offline)`.

## Développement

```sh
make test      # tests unitaires
make lint      # rustfmt + clippy, avertissements bloquants
make build     # binaire optimisé dans target/release/nikka
make generate  # régénère la grille du logo depuis assets/logo.png
```

Organisation du code :

| Chemin | Contenu |
|---|---|
| `src/main.rs` | CLI |
| `src/routine/` | orchestration et rendu des sections |
| `src/system.rs` | gestionnaires de paquets |
| `src/cleanup/` | cibles de purge |
| `src/news/` | lecteur RSS/Atom, classement, lancements, KEV |
| `src/skills/` | taxonomie, analyse, fils HN |
| `src/jobs/` | France Travail, Adzuna, OSRM, scoring |
| `src/ui/` | thème, bannière, spinners, prompts |
| `src/logo/` | logo pixel art et animation ; `grid.txt` est généré par `examples/logogen.rs` |
| `src/execx/` | exécution de commandes et sudo |
| `src/httpx/` | client HTTP et cache disque |
| `src/work.rs` | commande `work` : terminal, session tmux, panneaux |
| `src/config.rs`, `src/textutil.rs` | configuration, normalisation du texte |

L'outil a d'abord été écrit en Go puis porté en Rust : écarts de comportement et retour arrière dans [docs/rust-migration.md](docs/rust-migration.md).

## Limites connues

- **France Travail** :
  - La documentation de l'API « Offres d'emploi v2 » était inaccessible sur francetravail.io au moment du développement (« L'API 84 n'est pas disponible »).
  - Les paramètres utilisés (`commune`, `distance`, `grandDomaine`, `publieeDepuis`, `range`, `sort`) et le format de réponse proviennent de la documentation officielle archivée d'Emploi Store Dev.
  - Les points d'accès token et recherche répondent bien. L'intégration n'a pas pu être testée avec de vrais identifiants.
- **OSRM** : le serveur de démonstration public est limité à 1 requête par seconde, pour un usage raisonnable et non commercial. L'outil n'envoie qu'une requête par exécution.
- **Hacker News** : le tag Algolia `front_page` peut avoir quelques heures de retard.
- **LinkedIn** : le paramètre `distance` du lien de recherche est en miles (25 mi, soit environ 40 km).
- **work sans tmux** : seul kitty sait créer des panneaux depuis la ligne de commande. Ghostty sous Linux, Ptyxis et GNOME Terminal n'en ont pas le moyen ; Konsole charge des dispositions (`--layout`), mais leur format JSON n'est pas documenté. Ces terminaux reçoivent 4 fenêtres.
- **fwupd** : les mises à jour firmware restent manuelles par choix, car elles peuvent exiger un redémarrage, le secteur ou le capot ouvert.

## Sources des API

- France Travail, authentification : <https://francetravail.io/produits-partages/documentation/utilisation-api-france-travail/client-credentials>
- France Travail, paramètres de recherche (archive officielle) : <https://web.archive.org/web/20210115130530/https://www.emploi-store-dev.fr/portail-developpeur-cms/home/catalogue-des-api/documentation-des-api/api/api-offres-demploi-v2/rechercher-par-criteres.html>
- France Travail, quota : <https://www.data.gouv.fr/api/1/dataservices/api-offres-demploi/>
- Adzuna : <https://developer.adzuna.com/docs/search>, <https://developer.adzuna.com/docs/terms_of_service>
- OSRM : <https://project-osrm.org/docs/v5.24.0/api/>, <https://github.com/Project-OSRM/osrm-backend/wiki/Demo-server>
- Code INSEE et coordonnées : <https://geo.api.gouv.fr/communes?codePostal=33000&fields=nom,code,centre>
- Launch Library 2 : <https://ll.thespacedevs.com/2.3.0/>
- Spaceflight News API : <https://api.spaceflightnewsapi.net/v4/>
- CISA KEV : <https://www.cisa.gov/known-exploited-vulnerabilities-catalog>
- HN Algolia : <https://hn.algolia.com/api>

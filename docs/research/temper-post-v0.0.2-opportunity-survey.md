# Temper après v0.0.2 : carte d'opportunités

Date de l'exploration : 2026-07-28

## Résumé exécutif

Temper n'est plus un simple prototype PGO. Après v0.0.2, c'est un orchestrateur
expérimental capable de construire, mesurer, confirmer et promouvoir un binaire
Cargo avec une chaîne de preuves particulièrement défensive. Il possède aussi un
premier corpus versionné. Sa frontière reste néanmoins étroite : un binaire
Cargo hôte Linux GNU x86_64, une métrique de durée murale, un seul workload
opaque, trois candidats fixes et une intégration uniquement par processus.

La prochaine étape ne devrait être ni BOLT, ni AutoFDO, ni une réécriture de
Cargo. Une divergence plus fondamentale doit d'abord être résolue. Cargo charge
maintenant récursivement les fichiers déclarés par la clé stable `include`, alors
que Temper ne lit que les fichiers `.cargo/config*` directement découverts.
Temper remplace ensuite les rustflags Cargo par un
`CARGO_ENCODED_RUSTFLAGS` possédé pendant PGO. Un rustflag défini uniquement
dans un fichier inclus peut donc être actif pour la baseline et les stratégies
statiques, absent des deux phases PGO et néanmoins absent des différences
signalées par la parité schema 2. Le code source établit la possibilité. Une
reproduction ciblée doit déterminer le comportement exact et choisir entre une
inspection Cargo plus fidèle, un wrapper rustc transparent ou une frontière
supportée plus étroite.

Après cette correction de trajectoire, le problème produit prioritaire est le
contrat de workload. Le même workload et les mêmes données servent aujourd'hui
à entraîner PGO, sélectionner les candidats et confirmer le gagnant. La
confirmation prouve une répétabilité sur ce workload, pas une généralisation à
un trafic, un dataset ou un scénario indépendant. Étendre l'espace de stratégies
avant de séparer entraînement et évaluation augmenterait le risque de
sur-optimisation tout en rendant le search plus coûteux.

La trajectoire recommandée est donc :

1. prouver la fidélité de la configuration Cargo et de l'injection PGO ;
2. définir une évaluation indépendante, multi-scénario et toujours
   correctness-first ;
3. qualifier par le corpus le plus petit ajout de stratégies stables ;
4. ajouter une couche d'explication optionnelle sans contaminer le chemin de
   décision stable ;
5. réserver BOLT, AutoFDO, les internals Cargo/rustc et le build system intégré
   aux horizons moyen ou long terme.

## 1. Périmètre, méthode et révisions

### 1.1 Méthode

L'étude a utilisé uniquement les sources locales de Temper, Rust et Cargo. Aucun
build, test, benchmark ou commande de profiling n'a été exécuté. Aucune source
web n'a été consultée. Les `.codex/` non suivis de Rust et Cargo n'ont pas été
utilisés comme sources produit.

Le rapport précédent,
`docs/research/rust-cargo-codebase-survey.md`, a servi de point de départ. Ses
constats structurants ont été vérifiés, puis l'exploration s'est concentrée sur
les zones qu'il laissait ouvertes : espace d'optimisation post-v0.0.2,
observabilité, wrappers, configuration Cargo récente, limites du workload,
post-link et trajectoire vers un build system intégré.

Les marqueurs suivants séparent la nature des conclusions :

- **Fait vérifié** : comportement ou structure directement observé dans les
  sources ou les artefacts versionnés.
- **Interprétation** : conséquence architecturale déduite de plusieurs faits.
- **Opportunité** : direction possible pour Temper, pas une recommandation
  d'implémentation immédiate.
- **Inconnue** : point qui exige une reproduction, une mesure ou une recherche
  ciblée.

Les chemins sans préfixe absolu sont relatifs au dépôt nommé par la section :
Temper par défaut, puis Rust dans la carte Rust et Cargo dans la carte Cargo.

### 1.2 Révisions étudiées

| Dépôt | Branche et révision | Version identifiable | État local observé |
|---|---|---|---|
| Temper | `main`, `f1619f9201c2abe75043952fb278c0d0c2295034` | package `0.0.2`, schema 2 | aligné sur `origin/main`, worktree propre |
| Rust | `main`, `bf9944f0b8006b152ef4d5f408ae75a0dde3d044` | source `1.99.0`, canal `nightly` | aligné sur `origin/main`, seul `.codex/` non suivi |
| Cargo | `master`, `0158e40d8638a7de292b7242b1533caaf48cbe5f` | crate `0.100.0`, `git describe` `0.95.0-1307-g0158e40d8` | aligné sur `origin/master`, seul `.codex/` non suivi |

Sources : `Cargo.toml:1-4`, `/home/arthur/dev/rust/src/version:1`,
`/home/arthur/dev/rust/src/ci/channel:1`,
`/home/arthur/dev/cargo/Cargo.toml:147`.

Les preuves de corpus ont été produites antérieurement avec rustc et Cargo
1.97.1, LLVM 22.1.6 et un worktree Temper identifié comme dirty. Le HEAD étudié
est propre, mais il n'existe pas de rerun du corpus attribuable à ce HEAD propre.
Cette différence interdit une conclusion de release readiness.

## 2. État actuel et frontière réelle de Temper

### 2.1 Contrat produit

**Fait vérifié.** Le README définit Temper comme une toolchain expérimentale
d'optimisation qui doit mesurer un workload représentatif, chercher de
meilleures stratégies et ne conserver un binaire que si le gain est
reproductible. Ses principes sont la mesure, la connaissance du workload, la
compatibilité Cargo, l'explicabilité, la reproductibilité et la simplicité
(`README.md:1-76`).

**Fait vérifié.** La trajectoire publique sépare déjà trois temps :

1. prouver des gains runtime dans l'écosystème Cargo existant ;
2. approfondir l'intégration compilateur seulement si les benchmarks le
   justifient ;
3. explorer ensuite un build system moderne compatible avec `Cargo.toml` et
   `Cargo.lock` (`README.md:78-106`).

**Interprétation.** La valeur distinctive de Temper n'est pas l'accès à un flag
rustc particulier. C'est la boucle de décision expérimentale : identité des
inputs, workload, recherche bornée, confirmation indépendante, rejet
conservateur et provenance. Toute nouvelle stratégie qui affaiblit cette boucle
serait contraire à la vision, même si elle produit ponctuellement un binaire
plus rapide.

### 2.2 Implémentation effective

Le flux actuel est :

```text
preflight
  -> cargo metadata
  -> baseline isolée
  -> ThinLTO et FatLTO/CGU1 isolés
  -> screening
  -> build instrumenté PGO
  -> workload d'entraînement
  -> validation et merge des profils
  -> build PGO
  -> screening PGO
  -> sélection
  -> rebuild frais baseline + candidat
  -> confirmation appariée
  -> promotion atomique
  -> run.json + latest.json
```

**Fait vérifié.** `main::optimize` porte directement cet enchaînement
(`src/main.rs:52`, `src/main.rs:95`, `src/main.rs:163`,
`src/main.rs:228`, `src/main.rs:354`). `Strategy` est un enum fermé à quatre
identités, baseline incluse (`src/strategy.rs:23-54`). `BuildPlan` isole les
target directories et exprime LTO par configuration Cargo
(`src/strategy.rs:66-159`).

**Fait vérifié.** Cargo conserve la résolution, le graphe, les build scripts, le
scheduling, les fingerprints et les artefacts. Temper utilise
`cargo metadata --format-version 1`, puis
`cargo build --message-format=json`, et sélectionne exactement un
`compiler-artifact.executable` (`src/cargo.rs:139-176`,
`src/cargo.rs:264-487`).

**Fait vérifié.** Le workload est une commande directe sans shell. Temper fixe
`TEMPER_BINARY`, mesure la durée murale du processus entier, impose timeout,
bornes de sortie et process group Linux (`src/workload.rs:118-346`). Le même
`WorkloadSpec` est utilisé par `screen`, `train_pgo` et `confirm`
(`src/workload.rs:274-344`, `src/strategy.rs:376-414`,
`src/main.rs:163-169`).

**Fait vérifié.** Measurement-v1 fixe deux warmups, sept échantillons de
screening, vingt paires AB/BA, 10 000 bootstraps, un seuil pratique par défaut
de 2 % et une relative MAD maximale de 10 %
(`docs/measurement-v1.md:5-45`, `src/measurement.rs:83-194`).

**Fait vérifié.** Schema 2 ajoute la preuve de parité PGO, les rustflags
ordonnés, les identités d'outils, les diagnostics Cargo structurés et les
profils bruts/mergés hashés (`docs/schema-v2.md:1-72`). La persistance protège
aussi les interruptions, ENOSPC, copies, renames, syncs et publication de
`latest.json` (`src/run.rs:249-1051`, `src/anchored.rs:21-323`,
`src/promotion.rs:159-328`).

### 2.3 Preuves disponibles

**Fait vérifié.** Les deux PRD sont `DONE` : 16 stories pour v0.0.1 et 13 pour
v0.0.2. Le corpus v1 contient trois snapshots d'applications réelles
(`b3sum`, `xsv`, `hexyl`) et un contrôle synthétique. Douze `run.json` schema 2
sont retenus.

**Fait vérifié.** Sur les neuf runs d'applications réelles, seuls les trois runs
`xsv` ont confirmé un gain, tous avec PGO. Leurs ratios médians étaient environ
0,953, 0,963 et 0,952. Les runs BLAKE3 et `hexyl` n'ont rien promu. Aucun score
cross-application n'a été calculé
(`benchmarks/corpus/v1/results/reference/2026-07-28/summary.json`,
`docs/verification-v0.0.1.md:245-297`).

**Fait vérifié.** Le ledger décrit ces workloads comme des proxies locaux
bornés, pas comme du trafic de production. Les résultats proviennent d'un seul
hôte et d'un worktree dirty. Il n'existe ni corpus production-representative,
ni cible Cargo bench propre à Temper, ni preuve cross-machine.

**Interprétation.** Le milestone initial est partiellement atteint : Temper a
une preuve reproductible sur un cas réel épinglé, mais pas encore une preuve de
valeur généralisable. Trois applications et un seul hôte sont un excellent
harness de qualification, pas encore un fondement suffisant pour choisir une
architecture plus profonde.

### 2.4 Frontière réelle après v0.0.2

| Axe | Frontière réelle |
|---|---|
| Projet | workspace Cargo avec lockfile existant |
| Cible | exactement un binaire hôte `x86_64-unknown-linux-gnu` |
| Toolchain | Cargo/rustc externes, `llvm-profdata` adjacent au rustc |
| Build | release, `--locked`, target directory neuf par stratégie |
| Search | baseline, ThinLTO, FatLTO avec CGU 1, un PGO basé sur le meilleur pré-PGO |
| Workload | une commande de confiance, stateless du point de vue de Temper |
| Correction | exit 0 du workload, oracle supplémentaire à la charge du workload |
| Objectif | durée murale du processus workload entier |
| Décision | screening au median, confirmation AB/BA avec intervalle bootstrap |
| Sortie | binaire promu et rapport schema 2 sous `.temper/` |
| Stabilité | aucune promesse 0.x, production sur surfaces CLI stables seulement |

Temper n'est donc pas encore :

- un optimiseur multi-objectifs ou multi-workloads ;
- un outil pour services long-running, bibliothèques, tests ou benchmarks Cargo ;
- un système de sélection de CPU de déploiement ;
- un orchestrateur BOLT ou AutoFDO ;
- un profiler de compilation ;
- un remplaçant du resolver, des fingerprints ou du scheduler Cargo ;
- un build system intégré.

### 2.5 Dérive documentaire

**Fait vérifié.** `README.md` et `AGENTS.md` présentent encore v0.0.1 comme
état courant ou v0.0.2 comme travail planifié, alors que les deux status trackers
sont `DONE`, le package vaut 0.0.2 et le corpus existe.

**Interprétation.** Ce décalage n'est pas une nouvelle feature, mais une dette
de contrat. Il augmente le risque qu'une recherche, un utilisateur ou un futur
PRD parte d'une frontière obsolète. Il devra être corrigé séparément avant toute
publication, sans être confondu avec le prochain cycle produit.

## 3. Carte architecturale de Rust pertinente pour Temper

### 3.1 Pipeline

**Fait vérifié.** Le flux rustc pertinent commence dans `rustc_driver`, construit
la session et le contexte global, force analyses et MIR optimisé, collecte les
mono items, les partitionne en codegen units, délègue au backend, puis linke.
Les frontières principales sont :

- `rustc_interface::start_codegen` pour lancer l'encodage de metadata et le
  backend (`compiler/rustc_interface/src/passes.rs:1270`) ;
- `collect_and_partition_mono_items` pour la partition CGU
  (`compiler/rustc_codegen_ssa/src/base.rs:733`) ;
- `CodegenBackend::{codegen_crate, join_codegen, link}`
  (`compiler/rustc_codegen_ssa/src/traits/backend.rs:119-159`) ;
- `run_linker` pour l'exécution finale du linker
  (`compiler/rustc_codegen_ssa/src/back/link.rs:1061`).

**Interprétation.** Les choix de Temper se répartissent en quatre couches :

```text
Cargo profile et graphe
  -> rustc MIR/monomorphisation
  -> LLVM codegen/LTO/PGO
  -> linker
  -> post-link
```

Temper contrôle aujourd'hui une partie de la première couche et PGO dans la
troisième. Il n'observe presque rien des deuxième, troisième et quatrième
couches.

### 3.2 Leviers stables

| Levier | Fait vérifié | Pertinence pour Temper |
|---|---|---|
| `codegen-units` | stable, compromis parallélisme de build contre qualité du code (`src/doc/rustc/src/codegen-options/index.md:28-40`) | Temper ne teste que la valeur effective de la baseline et CGU 1 couplé à FatLTO |
| LTO thin/fat | stable, local ThinLTO implicite possible, propagation inter-crates (`codegen-options/index.md:376-409`) | déjà exploité, mais espace CGU/LTO très réduit |
| `target-cpu` | stable, `native` et CPUs explicites (`codegen-options/index.md:716-727`) | fort potentiel, mais exige un contrat de CPU de déploiement |
| `target-feature` | stable mais explicitement unsafe et potentiellement UB (`codegen-options/index.md:727-757`, `targets/known-issues.md:1-8`) | ne doit pas devenir un search automatique sans preuve de compatibilité binaire |
| linker et link args | stables, command line substituable (`compiler/rustc_session/src/options.rs:2245`) | utile pour build time et parfois layout runtime, dépendance externe importante |
| linker-plugin LTO | stable, délègue LTO au linker (`codegen-options/index.md:344-365`) | intéressant surtout pour graphes Rust/C mixtes, complexité élevée |
| PGO instrumenté | `-Cprofile-generate` et `-Cprofile-use` stables (`codegen-options/index.md:520-538`) | cœur actuel, encore limité par le contrat de workload |
| optimization remarks | `-Cremark` stable (`codegen-options/index.md:602-610`) | piste d'explication, pas contrat stable sur noms/textes LLVM |
| `llvm-profdata`, `llvm-size`, `llvm-objdump` | distribués par `llvm-tools-preview` (`src/bootstrap/src/lib.rs:61-77`) | données de profil, taille et désassemblage encore sous-exploités |

**Opportunité.** Le prochain espace stable à qualifier n'est pas une collection
arbitraire de flags. Les candidats plausibles sont un petit éventail CGU/LTO,
un CPU de déploiement explicitement déclaré et, sur besoin prouvé, un linker.
`panic=abort`, `target-feature`, `llvm-args` et les passes custom changent les
sémantiques, la compatibilité ou la stabilité de manière trop forte pour un
search automatique par défaut.

**Inconnue.** Le corpus actuel ne permet pas de savoir si un CGU intermédiaire,
un CPU explicite ou un linker produit un gain reproductible supérieur à leur
coût. Cette question doit être mesurée avec une évaluation indépendante avant
d'élargir `Strategy`.

### 3.3 Observabilité rustc

**Fait vérifié.** Les mécanismes fins restent nightly :

- `-Zself-profile` enregistre queries, cache hits, activité LLVM et tailles
  d'artefacts (`compiler/rustc_session/src/options.rs:2807-2824`,
  `compiler/rustc_data_structures/src/profiling.rs:619-668`) ;
- `--json=timings` exige `-Zunstable-options`
  (`compiler/rustc_session/src/config.rs:2550-2552`) ;
- `-Zllvm-time-trace` produit un JSON LLVM
  (`compiler/rustc_session/src/options.rs:2600`,
  `compiler/rustc_codegen_llvm/src/lib.rs:390-393`) ;
- AutoFDO utilise `-Zdebuginfo-for-profiling` et
  `-Zprofile-sample-use` (`compiler/rustc_session/src/options.rs:2419,2757`) ;
- `rustc_driver` et `rustc_public` ne sont pas des APIs stables de produit.

**Fait vérifié.** Le self-profile détaillé peut multiplier le temps de
compilation par trois à cinq pour certains événements
(`compiler/rustc_data_structures/src/profiling.rs:595-603`).

**Interprétation.** Une explication fine ne doit pas être collectée pendant
chaque candidat de production. Le meilleur modèle est une décision stable et
un diagnostic opt-in, attribué à un commit exact de rustc, avec son overhead
mesuré. Les données nightly peuvent expliquer une décision, mais ne doivent pas
être nécessaires pour promouvoir un binaire stable.

### 3.4 AutoFDO

**Fait vérifié.** AutoFDO évite le ralentissement de l'instrumentation et peut
profiler un workflow réel. Dans Rust, il exige cependant nightly, Linux x86_64,
`perf`, des données de branche et l'outil externe `create_llvm_prof`
(`src/doc/unstable-book/src/compiler-flags/debuginfo_for_profiling.md:1-38`).
Il est mutuellement exclusif avec PGO instrumenté
(`compiler/rustc_session/src/config.rs:2581-2587`).

**Opportunité.** AutoFDO pourrait un jour résoudre le problème des services ou
workloads trop coûteux à instrumenter.

**Interprétation.** Il ne convient pas au prochain cycle : privilèges perf,
outil externe, format de profil, nightly et attribution des binaires ajoutent
plusieurs frontières instables avant qu'un besoin corpus ne soit prouvé.

### 3.5 BOLT et post-link

**Fait vérifié.** BOLT n'est pas une phase applicative de rustc. `opt-dist`
instrumente un artefact déjà linké, collecte des profils, fusionne les fichiers
avec `merge-fdata`, puis réécrit l'artefact avec réordonnancement de blocs et de
fonctions, splitting et ICF (`src/tools/opt-dist/src/bolt.rs:9-99`,
`src/tools/opt-dist/src/training.rs:204-251`).

**Fait vérifié.** Le build doit conserver les relocations. Le chemin Rust
ajoute `-Wl,-q` pour `rustc_driver`
(`src/bootstrap/src/core/build_steps/compile.rs:1147-1151`,
`src/bootstrap/src/bin/rustc.rs:248-253`). `llvm-bolt` et `merge-fdata` ne
figurent pas dans les outils distribués par `llvm-tools-preview`
(`src/bootstrap/src/lib.rs:61-77`).

**Interprétation.** BOLT est cohérent avec la vision post-link de Temper et
pourrait compléter PGO sur des applications sensibles au layout. Mais il
introduit un toolchain LLVM externe, des contraintes ELF/relocations, une
mutation post-link, des questions de debug/unwind et une nouvelle chaîne de
provenance. Il appartient au moyen terme après une étude dédiée, pas au prochain
incrément par défaut.

### 3.6 Backends et internals

**Fait vérifié.** `CodegenBackend` sépare clairement codegen et link, mais reste
interne. Cranelift, GCC et LLVM n'offrent pas un contrat produit uniforme pour
Temper. Les queries, MIR, mono items, `.rmeta`, `.rlink` et work products
incrémentaux sont également des formats internes.

**Interprétation.** L'intérêt de ces internals est principalement explicatif ou
lié au futur objectif de temps de compilation. Ils ne résolvent pas le problème
produit actuel : démontrer plus souvent un gain runtime valable.

## 4. Carte architecturale de Cargo pertinente pour Temper

### 4.1 Pipeline et ownership

**Fait vérifié.** `CompileOptions` décrit l'intention CLI, puis `compile_ws`
résout packages, features et profils, génère les racines, étend le `UnitGraph`,
construit `BuildContext` et délègue à `BuildRunner`
(`/home/arthur/dev/cargo/src/ops/cargo_compile/mod.rs:75-203,320`,
`unit_generator.rs:50`).

**Fait vérifié.** Une `Unit` contient package, target, profil, mode, host/target,
features, rustflags et identité transitive
(`/home/arthur/dev/cargo/src/compiler/unit.rs:38-158`). `BuildRunner` possède
l'état mutable de compilation, les outputs de build scripts, les fingerprints
et la queue (`build_runner/mod.rs:35-225`).

**Interprétation.** Cette ownership valide la frontière actuelle :

- Cargo doit rester propriétaire du graphe et de sa fraîcheur ;
- Temper doit rester propriétaire de l'expérience, de la mesure et de la
  décision ;
- toute duplication de Cargo doit être justifiée par un manque précis et
  mesuré.

### 4.2 Surfaces externes stables

| Surface | Contrat réel | Usage Temper |
|---|---|---|
| `cargo metadata --format-version 1` | packages, targets, workspace, IDs opaques, évolution additive | déjà utilisé correctement |
| `--message-format=json` | diagnostics, artifacts, build scripts, fin de build | déjà utilisé et durci fail-closed |
| `--target-dir` | isolation stable des outputs | déjà utilisé par stratégie |
| `--config` | overrides documentés, stable depuis 1.63 | déjà utilisé pour les profils LTO |
| `RUSTC_WRAPPER` | intercepte toutes les invocations rustc | non utilisé, piste pour injection/observation |
| `RUSTC_WORKSPACE_WRAPPER` | intercepte seulement les membres du workspace et change le hash de filenames | non utilisé, utile pour observation ciblée mais insuffisant seul pour PGO de toutes les dépendances |
| `--timings` | HTML avec unités et concurrence, humain uniquement | archivable, mais impropre au parsing |

Sources :
`/home/arthur/dev/cargo/doc/book/src/reference/external-tools.md:32-252`,
`doc/book/src/reference/config.md:498-522`,
`doc/man/includes/options-timings.md:1-8`.

**Fait vérifié.** Cargo incorpore le wrapper dans l'identité rustc/fingerprint,
et le workspace wrapper sépare les noms d'artefacts
(`/home/arthur/dev/cargo/src/util/rustc.rs:320-348`,
`doc/book/src/reference/config.md:507-521`).

**Interprétation.** Un wrapper est une surface stable, mais pas une solution
gratuite. Il doit préserver argv, env, stdout, stderr, exit status et signaux,
distinguer unités host/target, composer les wrappers existants et rendre son
overhead mesurable. Son effet sur les fingerprints doit être inclus dans la
preuve de parité.

### 4.3 Configuration Cargo : finding prioritaire

#### Faits vérifiés

Cargo permet maintenant à un fichier de configuration d'en inclure d'autres via
la clé stable `include`. Les chemins sont relatifs au fichier parent, peuvent
être optionnels, sont chargés récursivement et fusionnés avant la configuration
englobante (`/home/arthur/dev/cargo/doc/book/src/reference/config.md:278-327`,
`src/context/mod.rs:1299-1455`, `src/context/mod.rs:2342-2394`). La fonctionnalité
a été stabilisée en 1.93 (`doc/book/src/reference/unstable.md:2430-2434`).

Temper, lui, découvre uniquement Cargo home puis les `.cargo/config` ou
`.cargo/config.toml` des ancêtres. Il lit chacun avec `toml::from_str`, mais ne
résout pas `include` (`src/strategy.rs:968-1059`,
`src/strategy.rs:1125-1166`).

Cargo donne priorité à `CARGO_ENCODED_RUSTFLAGS`, puis `RUSTFLAGS`, puis
`target.*.rustflags` et enfin `build.rustflags`
(`/home/arthur/dev/cargo/src/compiler/build_context/target_info.rs:758-834`).
Temper pose son propre `CARGO_ENCODED_RUSTFLAGS` pour les phases PGO
(`src/strategy.rs:141-157`).

#### Interprétation architecturale

Si un fichier inclus contient les rustflags target et que le fichier directement
découvert ne les contient pas :

1. Cargo applique ces rustflags à la baseline et aux candidats statiques.
2. Temper ne les voit pas dans `effective_target_rustflags`.
3. Les builds PGO reçoivent le canal environnemental prioritaire de Temper.
4. Ce canal remplace les rustflags Cargo par les seuls flags connus de Temper
   et le flag PGO.
5. Les deux phases PGO peuvent être cohérentes entre elles tout en divergeant de
   la baseline.
6. Schema 2 peut donc déclarer `matched: true` sans avoir observé l'input omis.

Cette conclusion est une implication directe du code, pas encore une
reproduction exécutée. Elle remet concrètement en question la frontière
stratégique acceptée en v0.0.2 : une réimplémentation partielle de la provenance
Cargo n'est pas durable face à l'évolution de surfaces pourtant stables.

#### Opportunité

Trois familles de solutions méritent comparaison, sans en choisir une ici :

1. suivre récursivement `include` et continuer à reproduire la fusion Cargo ;
2. observer les commandes rustc effectives pendant une phase de référence ;
3. injecter PGO au niveau d'un wrapper rustc transparent, après que Cargo a
   résolu sa configuration, sans substituer les rustflags.

La première minimise le changement local mais poursuit la duplication. La
troisième épouse mieux l'ownership Cargo, mais rend la composition avec
`sccache`, les wrappers existants, les unités host/target et les fingerprints
plus complexe. Une preuve ciblée doit trancher.

### 4.4 Profils, LTO et fingerprints

**Fait vérifié.** Le `Profile` Cargo effectif contient opt level, LTO, backend,
CGU, debuginfo, incremental, panic, strip et rustflags
(`/home/arthur/dev/cargo/src/workspace/profiles.rs:612-719`). Cargo calcule
ensuite les besoins LTO par unité et propage objets/bitcode dans le graphe.

**Fait vérifié.** Les fingerprints couvrent rustc, profil, target, features,
rustflags, dépendances et une partie de la configuration. Cargo précise
explicitement qu'ils ne capturent qu'une petite partie de l'environnement et
que la complétude exigerait hashing, sandboxing et traçage de fichiers plus
coûteux (`/home/arthur/dev/cargo/src/compiler/fingerprint/mod.rs:1-103`).

**Interprétation.** Temper a raison de déléguer LTO et la fraîcheur à Cargo. En
revanche, son target directory neuf par candidat signifie que les fingerprints
servent surtout à la correction intra-build, pas à amortir le search entre
candidats ou runs. Ce choix protège l'expérience mais rend tout élargissement
combinatoire très coûteux.

### 4.5 Observabilité Cargo

**Fait vérifié.** `--timings` stable ne fournit plus de format machine. Le
format JSON timings a été retiré en nightly 1.94
(`/home/arthur/dev/cargo/doc/book/src/reference/unstable.md:2305-2310`).

**Fait vérifié.** `-Zbuild-analysis` écrit des logs JSONL avec build, graphe,
unités, timings, sections et raisons de fingerprint dans `$CARGO_HOME/log/`.
`cargo report` peut restituer sessions, timings et rebuilds
(`doc/book/src/reference/unstable.md:1927-1971`,
`src/util/log_message.rs:16-181`). Aucun champ de version de schema n'est visible
dans `LogEntry`.

**Fait vérifié.** `--unit-graph` reste nightly mais expose un JSON version 1 et
ne compile rien (`doc/book/src/reference/unstable.md:735-790`).
`--build-plan` a été supprimé en nightly 1.93
(`doc/book/src/reference/unstable.md:2419-2423`).

**Interprétation.** `build-analysis` est la meilleure fenêtre actuelle sur le
futur Cargo, mais une mauvaise dépendance pour le cœur de Temper :

- nightly ;
- schema non contractualisé ;
- écriture globale sous Cargo home plutôt que sous `.temper/` ;
- couplage aux unités et fingerprints internes.

Une capability expérimentale pourrait l'archiver par toolchain exact. Elle ne
doit pas devenir une précondition du chemin stable ni être remplacée par un
parser du HTML `--timings`.

### 4.6 Cargo comme bibliothèque

**Fait vérifié.** La crate Cargo expose techniquement `ops`, `compiler`,
`Executor`, `Unit` et `BuildRunner`, mais sa bibliothèque est maintenue pour
Cargo lui-même et ne promet pas une API externe stable
(`/home/arthur/dev/cargo/src/lib.rs:1-40`,
`src/compiler/mod.rs:125-170`).

**Interprétation.** Embarquer Cargo ne supprimerait pas la complexité. Temper
devrait suivre les changements de structures, initialisation du contexte,
sources, jobserver, diagnostics, build scripts, locks et layouts. Cette option
n'est justifiée que si une information indispensable ne peut être obtenue par
processus et si une contribution upstream stable a échoué.

## 5. Analyse transversale Temper, Rust et Cargo

### 5.1 Répartition saine de l'ownership

| Responsabilité | Propriétaire actuel | Décision |
|---|---|---|
| résolution et features | Cargo | conserver |
| profiles et propagation LTO | Cargo | conserver |
| unités, build scripts, proc macros | Cargo | conserver |
| fingerprints, locks, scheduling | Cargo | conserver |
| MIR, mono items, CGU, LLVM | rustc | conserver |
| link | rustc et linker choisi | observer avant de contrôler davantage |
| workload et correctness oracle | utilisateur, encadré par Temper | approfondir |
| search et stratégie expérimentale | Temper | approfondir |
| mesure, confirmation, promotion | Temper | conserver et généraliser |
| provenance de l'expérience | Temper | conserver, compléter les inputs effectifs |

### 5.2 Capacités importantes non exploitées

#### Côté produit

- workloads d'entraînement et d'évaluation distincts ;
- résultats et seuils par scénario ;
- métriques autres que la durée murale du wrapper complet ;
- services warm, throughput, latence ou workload piloté ;
- déclaration explicite du CPU et de l'environnement de déploiement ;
- comparaison multi-host et variance inter-run ;
- capacité à expliquer pourquoi un candidat a gagné, pas seulement lequel.

#### Côté Rust

- continuum CGU/LTO stable ;
- `target-cpu` explicite ;
- linker et linker-plugin LTO ;
- optimization remarks ;
- inspection des profils avec `llvm-profdata` ;
- taille, sections et désassemblage avec les LLVM tools distribués ;
- BOLT, AutoFDO et self-profile comme capabilities expérimentales.

#### Côté Cargo

- wrapper rustc stable pour observation ou injection ;
- HTML `--timings` comme artefact humain ;
- `build-analysis` et section timings en mode nightly ;
- `unit-graph` comme laboratoire de planification ;
- nouveaux mécanismes de layout, locking et cache comme matière du build system
  long terme.

### 5.3 Limites susceptibles de devenir bloquantes

#### L1. Provenance Cargo partiellement réimplémentée

Le finding `include` montre que cette limite est déjà active. Le schema de
parité est aussi fiable que les inputs que Temper sait observer. Une égalité
entre deux projections incomplètes ne prouve pas l'égalité des builds réels.

#### L2. Entraînement, sélection et confirmation partagent le même workload

Le corpus appelle toujours la même distribution pondérée pour les trois phases.
Par exemple, `xsv` exécute 55 fois `stats` et 45 fois `select` à chaque
invocation (`benchmarks/corpus/v1/cases/xsv/workload.sh:27-41`). La confirmation
utilise de nouveaux échantillons et de nouveaux builds, mais pas de données ou
scénarios tenus à l'écart.

Cette architecture valide la répétabilité locale. Elle ne détecte pas une
amélioration spécialisée au workload d'entraînement qui régresse un autre
scénario important.

#### L3. Une seule métrique opaque

Temper mesure le processus workload entier. Startup, shell des fixtures, hash de
sortie, I/O et orchestration font partie du résultat. Cette simplicité est
robuste pour une CLI, mais devient bloquante pour services, métriques de
throughput, latence en régime établi ou objectifs secondaires comme taille et
mémoire.

#### L4. Search codé comme state machine unique

`main.rs`, `run.rs` et `strategy.rs` contiennent un traitement spécial du PGO et
des phases fixes. Cette rigidité protège le search borné actuel. Elle deviendra
un coût de changement important dès qu'une seconde stratégie entraînée ou
post-link sera justifiée. Il ne faut pas la refactorer spéculativement : la
prochaine stratégie prouvée doit définir l'abstraction minimale.

#### L5. Coût combinatoire

Chaque stratégie utilise un target directory neuf, puis le gagnant et la
baseline sont reconstruits encore une fois pour confirmation. Cette isolation
est statistiquement et opérationnellement saine, mais tout grid search naïf
multipliera les builds froids. Un futur planner devra raisonner sur valeur
attendue, coût et dépendances entre stratégies.

#### L6. Plateforme et déploiement confondus

Le CPU de mesure est implicitement le CPU de déploiement. Ajouter
`target-cpu=native` sans contrat explicite produirait un binaire potentiellement
inexécutable ailleurs. À l'inverse, rester sur le CPU générique limite l'un des
leviers explicitement cités dans le README.

#### L7. Contrat utilisateur inférieur à la target experience

Le README vise `temper optimize --workload "cargo bench"`. Le contrat actuel
exige une commande qui consomme `TEMPER_BINARY`. Un `cargo bench` ordinaire
construit et exécute ses propres artefacts et ne mesure donc pas automatiquement
le candidat Temper. Atteindre la target experience exige un adapter ou un
protocole de benchmark, pas simplement accepter une chaîne shell.

#### L8. Explicabilité limitée

Le rapport explique configuration, temps de build, samples et motifs de rejet.
Il ne relie pas un gain à des fonctions chaudes, à la couverture du profil, au
codegen, au link ou au layout. Cette limite n'empêche pas la correction, mais
elle freinera le diagnostic des non-gains et la sélection de stratégies.

## 6. Carte des opportunités par horizon

### 6.1 Prochain cycle produit

Le prochain cycle doit consolider la validité de l'optimiseur, pas commencer le
build system intégré.

| Opportunité | Alignement README | Valeur | Stabilité | Coût/risque | Priorité |
|---|---:|---:|---:|---:|---:|
| fidélité complète des inputs Cargo et injection PGO | très fort | empêche une fausse parité | surfaces stables, solution inconnue | moyen à élevé | P0 |
| séparation entraînement/évaluation et résultats par scénario | très fort | limite la sur-optimisation | interne à Temper | moyen | P1 |
| rerun propre et extension raisonnée du corpus | très fort | améliore la qualité des preuves | stable | coût de curation élevé | P1 |
| qualification d'un petit espace CGU/LTO/CPU | fort | peut augmenter le taux de gains | options stables | builds coûteux, portabilité CPU | P2 |
| convergence des contrats README/docs/status | fort | réduit la dérive de frontière | stable | faible, hors feature | P2 |

Ce cycle ne suppose pas encore qu'une nouvelle stratégie sera livrée. Il doit
produire les preuves permettant de choisir si le prochain PRD porte sur le
contrat de workload, la compatibilité Cargo ou un élargissement du search.

### 6.2 Moyen terme

| Opportunité | Condition d'entrée | Forme plausible |
|---|---|---|
| CPU de déploiement explicite | matrice de compatibilité et exécution sur CPU cible | profils générique versus microarchitecture déclarée |
| planner de stratégies borné | au moins une nouvelle famille prouvée | graphe de candidats avec coûts et prérequis, sans plugins publics |
| mode d'explication | overhead et formats qualifiés | wrapper stable, artefacts `--timings`, diagnostic nightly opt-in |
| workload pour services | besoin utilisateur réel et métrique définie | protocole setup/run/teardown ou driver persistant |
| BOLT Linux/ELF | gains corpus au-delà de PGO et tooling reproductible | stratégie post-link optionnelle, toolchain épinglée |
| widening Cargo targets | cas utilisateur établi | examples, benches ou adapters, toujours via artifacts Cargo |
| multi-host | hôtes contrôlés et identité de déploiement | réplication d'expérience, jamais agrégation aveugle |

### 6.3 Vision long terme du build system intégré

Le futur build system ne doit pas commencer par réimplémenter Cargo. La
séquence cohérente est :

1. Temper prouve un avantage d'optimisation sur un corpus crédible.
2. Il acquiert un planner de stratégies, des workloads riches et une provenance
   complète.
3. Il mesure alors quels coûts de Cargo empêchent réellement la boucle :
   builds froids, réutilisation, scheduling, cache ou observabilité.
4. Il utilise `unit-graph` et `build-analysis` comme laboratoires nightly.
5. Il demande ou contribue les surfaces stables manquantes upstream.
6. Il n'intègre ou ne remplace un sous-système que si cette frontière stable ne
   peut pas porter la valeur produit.

À cet horizon, les sous-systèmes pertinents sont le modèle `Unit`/`UnitGraph`,
la génération d'unités, `BuildContext`, `BuildRunner`, les fingerprints, le
jobserver, `JobQueue`, les layouts et la future infrastructure de cache. Ils
constituent une carte de dépendances, pas une liste à copier.

La différenciation plausible d'un build system Temper serait un build plan
piloté par evidence, capable de choisir et réutiliser des expériences
d'optimisation. Elle ne serait pas un resolver Cargo alternatif sans avantage
mesuré.

## 7. Risques, stabilité et dépendances

### 7.1 Hiérarchie de confiance des surfaces

| Niveau | Exemples | Politique recommandée |
|---|---|---|
| stable machine-readable | metadata v1, Cargo messages JSON, target dir, config CLI | cœur produit |
| stable non machine-readable | Cargo `--timings` HTML, remarks LLVM textuels | artefact humain, pas parser contractuel |
| stable mais dangereuse | `target-feature`, link args, wrappers | capability avec invariants et tests dédiés |
| nightly versionnée | `--unit-graph` v1 | laboratoire épinglé au toolchain |
| nightly non versionnée | build-analysis, section timings, self-profile | diagnostic opt-in, parser isolé |
| API interne | Cargo library, rustc_driver, queries, backends | long terme seulement sur manque démontré |
| outil externe | BOLT, merge-fdata, AutoFDO converter, linker tiers | provenance et compatibilité explicites |

### 7.2 Risques principaux

| Risque | Impact | Réduction |
|---|---|---|
| config effective différente de la projection Temper | faux invariant PGO, candidat incomparable | exploration P0 `include`/wrapper |
| overfit au workload d'entraînement | gain local, régression réelle | holdout et gates par scénario |
| explosion du nombre de builds | UX et coût prohibitifs | search borné et planner cost-aware |
| CPU-specific non portable | crash ou illegal instruction en production | CPU de déploiement déclaré et vérifié |
| wrapper incompatible avec sccache ou autre wrapper | perte de cache, build cassé | composition et pass-through testés |
| parser d'une sortie humaine ou nightly | breakage silencieux | versionner capability et fail closed |
| BOLT altère debug/unwind/layout | binaire rapide mais opérationnellement invalide | oracles binaires et symbolisation |
| corpus trop étroit | stratégie optimisée pour trois proxies | expansion par classes et scénarios |
| même hôte pour toutes les preuves | conclusions non transférables | réplication multi-host explicite |
| documentation obsolète | mauvais choix produit et usage incorrect | convergence avant publication |

### 7.3 Dépendances entre directions

```text
fidélité Cargo
  -> preuve PGO valide
      -> contrat train/evaluate
          -> qualification de stratégies
              -> planner borné
                  -> BOLT ou CPU-specific

observabilité stable/nightly
  -> explicabilité
  -> données pour le futur build system

preuve corpus crédible
  -> justification d'intégration plus profonde
  -> éventuelles surfaces upstream
  -> build system intégré
```

## 8. Explorations ciblées recommandées

### P0. Fidélité de configuration Cargo et injection PGO

**Question exacte.** Temper peut-il garantir que les phases baseline,
instrumentation et use voient les mêmes inputs rustc effectifs, y compris
`include`, cfg rustflags, variables d'environnement et wrappers, sans
réimplémenter durablement la fusion Cargo ?

**Hypothèse.** Le canal `CARGO_ENCODED_RUSTFLAGS` actuel perd au moins les
rustflags définis uniquement dans un fichier inclus. Un wrapper rustc possédé
peut préserver la résolution Cargo et ajouter les flags PGO aux seules unités
target, mais son interaction avec wrappers existants et fingerprints peut rendre
une autre solution préférable.

**Zones de code à examiner.**

- Temper : `src/strategy.rs:811-1166`, `src/cargo.rs:264-533`,
  `tests/ep001_v002_contract.rs`, `tests/ep002_matrix.rs`,
  `docs/schema-v2.md`.
- Cargo : `src/context/mod.rs:1299-1548`,
  `src/compiler/build_context/target_info.rs:754-879`,
  `src/util/rustc.rs:198-348`, `doc/book/src/reference/config.md:278-327`,
  wrappers dans `doc/book/src/reference/config.md:498-522`.
- Rust : forme des arguments target/host et fingerprint des codegen options.

**Preuve attendue.**

1. Fixtures avec include simple, imbriqué, optionnel, cycle rejeté, string/list
   rustflags, cfg rustflags, build rustflags et env target.
2. Capture des argv rustc réels de baseline, generate et use.
3. Démonstration de la présence ou perte du flag inclus sur l'implémentation
   actuelle.
4. Prototype expérimental comparant inspection récursive et wrapper, sans
   changer le produit.
5. Preuve que build scripts et proc macros ne reçoivent pas PGO.
6. Matrice avec aucun wrapper, `sccache`-like wrapper, workspace wrapper et
   wrappers imbriqués.
7. Mesure de l'overhead, des fingerprints et des hashes d'artefacts.

**Décision permise.** Conserver le canal environnemental avec un inspecteur
Cargo complet, migrer l'injection vers un wrapper transparent, demander une
surface Cargo upstream, ou réduire explicitement la frontière supportée.

### P1. Contrat workload : entraînement, évaluation et scénarios

**Question exacte.** Quel protocole minimal prouve qu'un candidat entraîné sur
un workload améliore une évaluation indépendante sans régresser un scénario
pré-déclaré ?

**Hypothèse.** Séparer `training_workload` et `evaluation_workload`, puis
appliquer des gates par scénario, détectera des candidats que la durée agrégée
actuelle accepterait malgré une régression localisée.

**Zones de code à examiner.**

- `src/workload.rs:118-344`, `src/measurement.rs:83-194`,
  `src/main.rs:120-344`, `src/strategy.rs:376-414`.
- `docs/measurement-v1.md`, `docs/schema-v2.md`.
- `benchmarks/corpus/v1/manifest.json`, les quatre workloads et
  `scripts/run-corpus-v1.sh`.
- Rust PGO : `src/doc/rustc/src/profile-guided-optimization.md:20-137`.

**Preuve attendue.**

1. Pour chaque cas corpus, partition pré-déclarée des scénarios ou datasets en
   train et évaluation.
2. Trois runs ou plus par partition sur un commit propre.
3. Ratios et intervalles par scénario, plus un objectif primaire pré-déclaré.
4. Recherche d'au moins un cas où le résultat agrégé et un scénario divergent.
5. Mesure de l'overhead du harness et vérification que l'oracle reste hors de la
   portion mesurée ou est symétrique et négligeable.
6. Simulation du taux de faux rejet/promotion si plusieurs gates sont ajoutés.

**Décision permise.** Créer ou non un workload/schema/measurement v2, définir la
sémantique d'un holdout et décider si la confirmation reste globale ou devient
multi-scénario.

### P2. Frontière de stratégies stables justifiée par les preuves

**Question exacte.** Quel est le plus petit ensemble additionnel de stratégies
stables qui augmente le taux de gains confirmés sur des évaluations tenues à
l'écart, sans coût de build ou risque de déploiement disproportionné ?

**Hypothèse.** Quelques points CGU/LTO et un `target-cpu` explicitement lié au
déploiement fourniront plus de valeur qu'un grid search général. Il est possible
que le corpus conclue qu'aucun ajout n'est encore justifié.

**Zones de code à examiner.**

- Temper : `src/strategy.rs:23-159`, sélection `src/strategy.rs:1481-1506`,
  state machine `src/main.rs:52-407`, modèle `src/run.rs:155-258`.
- Cargo : `src/workspace/profiles.rs:612-719`,
  `src/compiler/lto.rs`, fingerprints et target dirs.
- Rust : codegen options `codegen-units`, LTO, `target-cpu`, linker,
  optimization remarks.
- Corpus v1 puis cas supplémentaires retenus.

**Preuve attendue.**

1. Matrice pré-enregistrée, petite et non adaptative pour éviter le cherry
   picking.
2. Build duration, artifact size, screening et confirmation par cas/scénario.
3. Réplication sur au moins deux classes supplémentaires ou un corpus v2.
4. Pour CPU-specific, exécution sur chaque CPU de déploiement déclaré et rejet
   sur incompatibilité.
5. Comparaison du coût total de search au gain confirmé.

**Décision permise.** Ajouter exactement une famille de candidats, conserver le
search actuel, ou introduire un planner interne borné. La preuve doit aussi
définir l'abstraction minimale, plutôt qu'un plugin system générique.

### P3. Enveloppe d'explicabilité stable et nightly

**Question exacte.** Quelles informations permettent d'expliquer un gain ou un
non-gain sans modifier la décision, ralentir tous les builds ou dépendre d'un
schema instable ?

**Hypothèse.** Un niveau stable peut retenir argv effectifs, build duration,
taille/sections et statistiques `llvm-profdata`. Un second niveau opt-in peut
collecter Cargo build-analysis, self-profile ou LLVM time trace sur le seul
candidat diagnostiqué.

**Zones de code à examiner.**

- Temper : `src/cargo.rs`, `src/run.rs`, reporting dans `src/main.rs`,
  raw profile records dans `src/strategy.rs`.
- Cargo : `src/util/log_message.rs`, `-Zbuild-analysis`,
  `-Zsection-timings`, `--timings`.
- Rust : self-profile, `--json=timings`, `-Cremark`,
  `-Zllvm-time-trace`, `llvm-profdata show`, `llvm-size`.

**Preuve attendue.**

1. Tableau champ par champ : format, stabilité, toolchain identity, overhead,
   emplacement et taille.
2. Rerun diagnostique d'un gain `xsv` et de deux non-gains.
3. Corrélation utile entre l'explication et une hypothèse de stratégie.
4. Preuve que désactiver l'explication ne change aucun build input ni hash du
   chemin de décision.
5. Parser fail-closed pour toute donnée nightly, isolé du schema stable.

**Décision permise.** Ajouter un mode `explain` expérimental, archiver seulement
des artefacts humains, demander une sortie Cargo stable, ou différer
l'explicabilité fine.

### P4. Viabilité BOLT sur le périmètre Linux/ELF

**Question exacte.** BOLT produit-il un gain incrémental reproductible au-dessus
du meilleur candidat LTO/PGO de Temper sur des binaires applicatifs, avec une
toolchain, des relocations et une symbolisation maîtrisées ?

**Hypothèse.** BOLT aidera certains workloads sensibles au layout, mais son
tooling externe et ses contraintes opérationnelles réduiront fortement son
périmètre supportable.

**Zones de code à examiner.**

- Rust `src/tools/opt-dist/src/bolt.rs`,
  `src/tools/opt-dist/src/training.rs`,
  `src/bootstrap/src/bin/rustc.rs:248-253`,
  distribution LLVM tools dans `src/bootstrap/src/lib.rs:61-77`.
- Link final dans `compiler/rustc_codegen_ssa/src/back/link.rs`.
- Temper : `BuildPlan`, promotion atomique, checksums, diagnostics, process
  lifecycle et corpus.

**Preuve attendue.**

1. Inventaire reproductible de `llvm-bolt` et `merge-fdata`, versions et hashes.
2. Build avec relocations, instrumentation, training, merge et rewrite dans un
   répertoire isolé.
3. Oracles fonctionnels, unwind/backtrace, debug info, permissions et checksum.
4. Mesures par scénario contre baseline, PGO et BOLT-sur-PGO.
5. Taille, coût de build, coût d'entraînement et taux d'échec par binaire.
6. Documentation exacte des formats ELF et architectures acceptés.

**Décision permise.** Placer BOLT dans un cycle moyen terme borné, le limiter à
une capability avancée ou l'écarter faute de valeur nette.

## 9. Pistes séduisantes à ne pas poursuivre maintenant

### Embarquer Cargo ou forker Cargo

Le gain immédiat serait l'accès à `Unit`, `BuildRunner`, fingerprints et
`Executor`. Le coût serait un couplage sans stabilité à presque toute la
complexité Cargo. Aucun manque actuel n'a encore résisté à une exploration de
surface stable ou à une contribution upstream.

### Intégrer rustc_driver ou les queries

Cette voie donne MIR, mono items, codegen et queries, mais exige rustc_private,
rustc-dev, la même toolchain et des adaptations fréquentes. Elle n'améliore pas
directement la validité des workloads ni le taux de gains prouvés.

### Parser `cargo --timings`

Cargo documente explicitement l'HTML comme humain uniquement et a supprimé son
ancien JSON. Le parser serait une dette certaine.

### Ressusciter `--build-plan`

La surface a été supprimée. `unit-graph` et `build-analysis` sont les laboratoires
actuels, avec leurs propres limites nightly.

### Search automatique de `target-feature` ou `llvm-args`

`target-feature` peut créer de l'UB ou une incompatibilité binaire entre crates.
`llvm-args` n'a pas les garanties de stabilité rustc. Une vitesse mesurée ne
compense pas ce risque.

### AutoFDO comme prochaine feature

Nightly, `perf`, privilèges, données de branche, outil de conversion externe et
format de profil ajoutent trop de variables avant que le corpus n'en démontre
le besoin.

### BOLT sans étude dédiée

L'absence de BOLT dans `llvm-tools-preview`, les relocations et la réécriture de
l'artefact en font une nouvelle pipeline, pas une variante simple de PGO.

### Plugin system ou search space dynamique

Le modèle fermé actuel évite un combinatoire incontrôlé et rend le schema
auditable. Il ne faut créer une abstraction qu'après qu'une nouvelle famille de
stratégies a prouvé sa valeur.

### Widening macOS, Windows, musl ou cross-compilation

Chaque plateforme change process groups, linkers, LLVM tools, formats
d'artefacts, PGO et promotion. La validité du workload et la fidélité Cargo sont
des prérequis plus fondamentaux.

### Remote cache, scheduler ou build system intégré

Ces axes servent surtout le temps de build, explicitement long terme dans le
README. Ils deviendront pertinents quand le coût du search validé sera le
principal frein produit, pas avant.

### Accepter directement une chaîne `cargo bench`

Un benchmark Cargo standard ne consomme pas automatiquement `TEMPER_BINARY`.
Accepter une chaîne shell ne résout ni l'identité de l'artefact ni le protocole
de mesure. Il faut d'abord définir un adapter explicite.

## 10. Recommandation pour la prochaine exploration

L'exploration P0 sur la fidélité Cargo doit être lancée en premier.

Elle a la meilleure priorité selon les six critères demandés :

| Critère | Évaluation |
|---|---|
| alignement README | maximal : Cargo-compatible, reproducible, explainable |
| valeur potentielle | protège la validité de tous les résultats PGO |
| qualité des preuves | forte : divergence visible directement dans les deux codebases |
| coût | borné : fixtures et capture d'argv avant tout changement produit |
| risque architectural | élevé si ignoré, maîtrisable si exploré maintenant |
| dépendance instable | aucune pour reproduire, `include` et wrappers sont stables |

Cette exploration peut invalider une hypothèse structurante de v0.0.2 :
schema 2 prouve la parité des inputs connus, pas nécessairement celle des
commandes rustc effectives. Ajouter des stratégies ou élargir le corpus avant de
fermer ce point amplifierait une preuve potentiellement incomplète.

Le contrat workload vient immédiatement après. Même une pipeline PGO
parfaitement fidèle ne garantit pas qu'un gain entraîné et confirmé sur le même
workload se généralise.

## Synthèse finale

**À explorer ensuite :** la fidélité de la configuration Cargo et une éventuelle
injection PGO par wrapper rustc transparent, avec reproduction explicite du cas
`include`.

**Pourquoi :** c'est le seul finding qui menace directement l'invariant de
comparabilité déjà revendiqué par Temper, sur une surface Cargo stable et
présente dans les toolchains supportées.

**À écarter pour le moment :** réécriture de Cargo, rustc internals, AutoFDO,
BOLT livré sans étude, plugins dynamiques, cross-platform et build system
intégré.

**Incertitudes ouvertes :** comportement exact du cas `include`, composition
des wrappers réels, généralisation hors workload d'entraînement, meilleur petit
espace de stratégies stables, valeur de l'explicabilité fine et gains BOLT
au-dessus de PGO.

# Cartographie large de Rust et Cargo pour Temper

Date de l'exploration : 2026-07-28

Ce document prépare le développement post-v0.0.1 de Temper. Il cartographie les
deux codebases en lecture seule, identifie leurs frontières d'intégration et
relie leurs mécanismes à l'implémentation actuelle de Temper. Il ne constitue
pas encore une analyse exhaustive de chaque sous-système.

## Périmètre et versions

| Dépôt | Révision explorée | État local observé |
|---|---|---|
| Rust | `/home/arthur/dev/rust` à `bf9944f0b8006b152ef4d5f408ae75a0dde3d044` | branche `main`, seul `.codex/` non suivi |
| Cargo | `/home/arthur/dev/cargo` à `0158e40d8638a7de292b7242b1533caaf48cbe5f` | branche `master`, seul `.codex/` non suivi |
| Temper | `/home/arthur/dev/temper` à `024b3899fd64a25084eaeff33301acf76b5f4d94` pour le dernier commit | changements documentaires locaux déjà présents |

Aucun build ni test de Rust ou Cargo n'a été exécuté. Les conclusions reposent
sur le code, les tests et la documentation présents dans ces révisions. Les
deux `.codex/` non suivis n'ont été ni lus comme source produit, ni modifiés.

## Synthèse

La frontière actuelle de Temper est la bonne :

1. Cargo reste le propriétaire de la résolution, des profils, du graphe de
   compilation, des build scripts, des fingerprints, du scheduling et des
   invocations de rustc.
2. rustc reste le propriétaire des queries, du MIR, de la monomorphisation, des
   codegen units, de LLVM, du PGO et du link final.
3. Temper orchestre des processus externes, impose des répertoires isolés,
   injecte uniquement des configurations documentées et consomme les sorties
   machine de Cargo.

Importer Cargo comme bibliothèque ou embarquer `rustc_driver` donnerait plus
d'observabilité et de contrôle, mais créerait immédiatement un couplage à des
API explicitement instables. Aucun besoin actuel de Temper ne justifie ce coût.

```text
Temper
  |
  +-> cargo metadata --format-version 1
  |     -> workspace, packages, targets, package IDs
  |
  +-> cargo build --message-format=json
        |
        +-> CLI -> ops -> résolution/profils -> UnitGraph
        |                                   -> BuildContext
        |                                   -> BuildRunner
        |                                   -> fingerprints + JobQueue
        |
        +-> une invocation rustc par Unit
              -> driver/interface
              -> queries et analyses
              -> HIR -> MIR optimisé
              -> monomorphisation -> codegen units
              -> backend LLVM -> objets -> link
        |
        +-> compiler-artifact JSON -> exécutable retenu par Temper
        +-> PGO : .profraw -> llvm-profdata -> .profdata -> rebuild
```

Les trois conclusions prioritaires pour la suite sont :

- conserver l'intégration externe et stable comme contrainte architecturale ;
- auditer et durcir immédiatement la cohérence PGO ;
- explorer l'observabilité de compilation séparément de l'optimisation runtime,
  car Cargo et rustc n'exposent pas le même niveau de stabilité.

## Cargo

### Organisation du dépôt

Le binaire se trouve sous `src/bin/cargo`, mais les commandes sont des adaptateurs
minces vers `src/ops`. La compilation converge vers
`src/ops/cargo_compile/mod.rs`. Le moteur est ensuite réparti entre :

- `src/workspace` pour les manifests, packages, targets et profils ;
- `src/resolver` et `src/ops/resolve` pour dépendances et features ;
- `src/compiler/build_context` pour l'état immuable calculé par le front-end ;
- `src/compiler/build_runner` pour l'exécution mutable ;
- `src/compiler/unit.rs` et `unit_dependencies.rs` pour le graphe réel des
  invocations ;
- `src/compiler/fingerprint` pour la fraîcheur et l'invalidation ;
- `src/compiler/job_queue` pour le scheduling et le jobserver ;
- `src/compiler/layout.rs` et `compilation_files.rs` pour les emplacements et
  noms d'artefacts ;
- `src/util/machine_message.rs` pour la sortie JSON publique.

Le commentaire d'architecture de `src/ops/cargo_compile/mod.rs:1-26` donne le
chemin nominal complet. Une `Unit` correspond à une invocation du compilateur,
puis les unités racines sont étendues en `UnitGraph`. `BuildContext` termine le
front-end, `BuildRunner` prépare les layouts et fingerprints, puis `JobQueue`
exécute les feuilles du graphe jusqu'à épuisement.

### Pipeline de compilation

1. La CLI construit le contexte global, résout alias et commande, puis appelle
   une opération.
2. `compile_ws` sélectionne les packages et targets, résout dépendances,
   features et profils, puis télécharge les packages requis.
3. `UnitGenerator` produit les unités racines. Une unité porte au minimum le
   package, le target Cargo, le profil, le mode, les features, la destination
   host ou target et les flags.
4. La marche des dépendances produit le graphe complet. Ce graphe est plus riche
   que `cargo metadata` : il exprime les invocations réelles, les variantes de
   profil, les build scripts, les proc macros et la séparation host/target.
5. `BuildRunner` calcule sorties et fingerprints, prépare les jobs, puis draine
   la queue.
6. Les commandes rustc sont assemblées avant exécution, puis enrichies par les
   résultats de build scripts.
7. `Compilation` collecte les exécutables et autres sorties nécessaires aux
   commandes `run`, `test` et `bench`.

Références principales :

- `src/ops/cargo_compile/mod.rs:1-26,129-203`
- `src/ops/cargo_compile/unit_generator.rs:772`
- `src/compiler/unit.rs:45-89`
- `src/compiler/build_context/mod.rs:52`
- `src/compiler/build_runner/mod.rs:35,169-225`
- `src/compiler/mod.rs:125-170,780-831`

### Profils, LTO et propagation dans le graphe

Cargo ne traduit pas simplement `profile.release.lto` en un unique `-Clto`.
`src/compiler/lto.rs` calcule un besoin par unité :

- exécuter Thin ou Fat LTO sur certaines unités ;
- produire uniquement du bitcode pour une dépendance consommée par LTO ;
- produire objet et bitcode lorsqu'ils sont tous les deux nécessaires ;
- produire uniquement un objet lorsque LTO ne s'applique pas ;
- désactiver LTO pour les unités host telles que build scripts et proc macros.

Le besoin se propage dans le graphe et fusionne lorsqu'une unité est atteinte par
plusieurs chemins. C'est une validation forte du choix actuel de Temper :
configurer `profile.release.lto` et `profile.release.codegen-units` via
`cargo --config`, plutôt qu'injecter directement `-Clto` dans rustc.

Référence : `src/compiler/lto.rs:9-69,92-150`.

### Configuration et rustflags

Cargo découvre les `.cargo/config` et `.cargo/config.toml` depuis le répertoire
courant vers ses ancêtres, puis consulte Cargo home. Les valeurs sont fusionnées
avec leur provenance. Les arguments `--config` sont ensuite fusionnés avec la
priorité CLI.

La sélection effective des rustflags suit cet ordre :

1. `CARGO_ENCODED_RUSTFLAGS` ;
2. `RUSTFLAGS` ;
3. `target.<triple>.rustflags`, puis les `target.'cfg(...)'.rustflags`
   correspondants ;
4. `build.rustflags`.

Avec un `--target` explicite, les artefacts host suivent un chemin distinct et
n'héritent pas des rustflags target. C'est précisément le comportement requis
pour ne pas instrumenter build scripts et proc macros pendant le PGO.

Références :

- `src/context/mod.rs:1148-1179,1350-1369,1551-1638,1693-1720`
- `src/context/config_value.rs:140-220`
- `src/compiler/build_context/target_info.rs:754-831,834-879,911-930`

### Fingerprints, layouts et isolation

Le fingerprint d'une unité couvre notamment rustc, les features déclarées et
activées, le target, le profil, les rustflags, la configuration, le type de
compilation, les dépendances, les sources et certains éléments d'environnement.
Cargo conserve aussi une raison explicite lorsqu'une unité devient dirty.

Les layouts séparent sorties finales, `deps`, incremental, build scripts et
`.fingerprint`. Les détails de nommage et certains nouveaux layouts restent
internes.

Conséquence pour Temper : un target directory distinct par stratégie et par
confirmation est une isolation correcte et robuste. Partager le même target
directory serait théoriquement invalidé par les fingerprints, mais augmenterait
le risque de contamination, de réutilisation inattendue et de conflit de lock.

Références :

- `src/compiler/fingerprint/mod.rs:1040-1110`
- `src/compiler/layout.rs:1-32,217-240`
- `src/compiler/build_runner/compilation_files.rs`

### Sorties machine et frontières stables

`compiler-artifact` contient `package_id`, `manifest_path`, le target, un résumé
du profil, les features, les filenames, un exécutable optionnel et `fresh`.
Cargo émet aussi `compiler-message`, `build-script-executed` et
`build-finished`.

Cette sortie est la meilleure source d'identité d'artefact pour Temper. Lire le
layout `target/` reproduirait une logique interne susceptible de changer.

`cargo metadata --format-version 1` est la bonne source pour workspace,
packages, membres et targets. Elle ne décrit pas tout le graphe d'unités de
compilation. `--unit-graph` expose ce graphe, mais reste explicitement instable.

Cargo contient un trait `Executor` capable d'intercepter les commandes rustc.
Il est tentant pour Temper, mais appartient à Cargo en tant que bibliothèque.
`src/lib.rs:10-19` indique explicitement que cette bibliothèque est maintenue
principalement pour Cargo et peut casser ses API ou hypothèses d'invocation.

Références :

- `src/util/machine_message.rs:10-104`
- `src/ops/cargo_metadata.rs:15-76`
- `src/compiler/unit_graph.rs:1-3`
- `src/util/command_prelude.rs:414,819-826`
- `src/compiler/mod.rs:125-170`
- `src/lib.rs:1-40`

### Tests et benchmarks Cargo

Les tests d'intégration forment une grande testsuite process-isolated autour de
`#[cargo_test]` et `cargo-test-support`. Le test PGO réel :

- construit une cible instrumentée ;
- l'exécute ;
- fusionne le profil avec le `llvm-profdata` du toolchain ;
- reconstruit avec `-Cprofile-use` ;
- exige `-Cllvm-args=-pgo-warn-missing-function`.

Les benchmarks Criterion couvrent surtout le resolver, l'initialisation de
workspace et le global cache tracker. Leur outil de capture conserve les
manifests locaux et le lockfile de workspaces réels dans des archives
déterministes, mais retire les targets, profils et sources exécutables. C'est un
bon modèle pour un corpus de topologies Cargo, pas pour le futur corpus runtime
de Temper.

Références :

- `tests/testsuite/main.rs:1-35`
- `tests/testsuite/pgo.rs:34-111`
- `benches/README.md:1-45`
- `benches/capture/src/main.rs:1-115`
- `benches/benchsuite/src/lib.rs:20-38`

## Rust

### Organisation du dépôt et bootstrap

Rust n'est pas un unique workspace Cargo. Le dépôt contient plusieurs
frontières :

- `compiler/` pour rustc ;
- `library/` pour le standard library et le sysroot ;
- `src/bootstrap/` pour le système de build ;
- `src/tools/` pour rustdoc, compiletest, rust-analyzer, opt-dist et les autres
  outils ;
- `src/llvm-project/` pour LLVM ;
- `tests/` pour les suites compilateur.

`x.py` délègue au bootstrap Python, qui obtient stage0, construit le bootstrap
Rust et orchestre les étapes mises en cache. La progression conceptuelle est :

- stage0 : toolchain beta téléchargé ;
- stage1 : compilateur des sources courantes construit par stage0 ;
- stage2 : compilateur auto-hébergé, utilisé notamment pour distribution et
  mesures représentatives du toolchain final.

Bootstrap est donc le control plane de la distribution Rust, pas le chemin
d'intégration approprié pour optimiser une application utilisateur.

Références :

- `x.py:43-53`
- `src/bootstrap/README.md:22-41,123-169`
- `src/bootstrap/src/core/builder/mod.rs:41-69,105-124`
- `src/doc/rustc-dev-guide/src/compiler-src.md:14-53,169-193`

### Pipeline rustc

1. `compiler/rustc/src/main.rs` entre dans `rustc_driver`.
2. Le driver parse les arguments, construit `rustc_interface::Config` et
   initialise la session.
3. `create_and_enter_global_ctxt` construit identité de crate, outputs,
   dep-graph, metadata store, arenas, providers de queries et `TyCtxt`.
4. Expansion et résolution produisent le programme résolu.
5. Les analyses forcent type checking, privacy et lints.
6. HIR et THIR descendent vers MIR, puis `optimized_mir` applique les passes
   nécessaires au codegen.
7. La monomorphisation collecte et partitionne les items en codegen units.
8. Le backend effectue `codegen_crate`, rejoint le codegen asynchrone, puis
   linke les modules compilés.
9. Le backend LLVM abaisse vers LLVM IR, optimise, effectue le LTO demandé,
   émet les objets et lance le link final.

Références :

- `compiler/rustc_driver_impl/src/lib.rs:170-230,265-341`
- `compiler/rustc_interface/src/passes.rs:928-1012,1184-1251,1270-1324`
- `compiler/rustc_mir_transform/src/lib.rs:800-846`
- `compiler/rustc_codegen_ssa/src/base.rs:733-748`
- `compiler/rustc_codegen_ssa/src/traits/backend.rs:107-159`
- `compiler/rustc_codegen_llvm/src/lib.rs:374-437`

### Queries et compilation incrémentale

Le compilateur est organisé autour de queries demand-driven. Une query associe
une clé à une valeur, mémorise le résultat, l'intègre au dep-graph et peut
persister certains résultats pour une compilation suivante. Les providers sont
enregistrés dans une table générée, avec des providers distincts possibles pour
les crates locales et externes.

Lorsqu'un nœud incrémental est green, rustc recharge un résultat sérialisé ou
réexécute le provider selon la nature de la query. Les work products de codegen
et les métadonnées incrémentales ont leur propre cycle de sauvegarde.

Cette architecture ouvre une future observabilité très fine pour Temper, mais
pas via une API stable. Elle est aujourd'hui accessible surtout par les outils
de self-profile et les options nightly.

Références :

- `compiler/rustc_middle/src/queries.rs:2-18,126-137`
- `compiler/rustc_query_impl/src/execution.rs:440-548`
- `compiler/rustc_interface/src/queries.rs:90-117`
- `src/doc/rustc-dev-guide/src/query.md:1-44,68-118`

### Métadonnées, codegen et link

Les métadonnées de crate peuvent être encodées avant la fin du codegen et
intégrées aux sorties. Les `.rmeta` utilisent un fichier temporaire puis un
rename pour éviter les lectures partielles. `-Zno-link` peut sérialiser modules,
crate info, métadonnées et noms de fichiers dans un `.rlink`.

Le trait interne `CodegenBackend` sépare explicitement :

- `codegen_crate` ;
- `join_codegen` ;
- `link`.

Cette séparation permet LLVM, GCC et Cranelift, mais ce n'est pas une interface
produit stable pour Temper. Les formats `.rmeta`, `.rlink` et les work products
incrémentaux ne doivent pas devenir des contrats externes sans besoin précis.

Références :

- `compiler/rustc_metadata/src/fs.rs:20-46,74-111`
- `compiler/rustc_interface/src/queries.rs:18-62,119-152`
- `compiler/rustc_codegen_ssa/src/traits/backend.rs:107-159`

### PGO

Le workflow applicatif officiel confirme les choix fondamentaux de Temper :

1. build avec `-Cprofile-generate` ;
2. exécution d'un workload représentatif ;
3. fusion des `.profraw` par `llvm-profdata` ;
4. rebuild avec `-Cprofile-use`.

Rust précise que :

- plusieurs binaires ou bibliothèques instrumentés peuvent produire plusieurs
  `.profraw` ;
- les flags rustc doivent rester identiques entre génération et utilisation,
  hors changement du flag PGO lui-même ;
- les chemins PGO doivent être absolus ;
- les profils d'une session antérieure doivent être isolés ou supprimés ;
- `cargo --target` empêche l'instrumentation des build scripts ;
- `-Cllvm-args=-pgo-warn-missing-function` est recommandé pour rendre visibles
  les fonctions sans données de profil.

Temper accepte plusieurs profils bruts, emploie des chemins absolus, isole les
données par run et sépare host/target. La parité exacte des flags reste à
auditer. Il ne passe actuellement pas l'avertissement LLVM.

Le PGO de bootstrap est un sujet distinct : il optimise rustc, rustdoc, Cargo ou
LLVM lors de la construction du toolchain, principalement quand stage1 construit
stage2. `src/tools/opt-dist` orchestre aussi LTO, PGO et BOLT pour la distribution
Rust et entraîne rustc sur rustc-perf. C'est une source de patterns pour la
gestion d'un corpus, pas une API à intégrer.

Références :

- `src/doc/rustc/src/profile-guided-optimization.md:20-137`
- `compiler/rustc_session/src/options.rs:2294-2297,2757`
- `compiler/rustc_session/src/config.rs:2577-2616`
- `compiler/rustc_codegen_ssa/src/back/write.rs:78-82,175-182`
- `compiler/rustc_codegen_llvm/src/back/write.rs:495-525,608-625,791-810`
- `src/bootstrap/src/core/config/toml/pgo.rs:13-32`
- `src/bootstrap/src/core/builder/cargo.rs:1537-1565`
- `src/tools/opt-dist/README.md:1-7`
- `src/tools/opt-dist/src/training.rs:108-199`

### Tests, perf et surfaces publiques

`compiletest` découvre et exécute les suites sous `tests/`. `run-make` est la
frontière d'intégration arbitraire pour les scénarios qui ont besoin de piloter
fichiers, commandes et linkers. rustc-perf porte les scénarios Check, Debug,
Opt et incremental utilisés pour mesurer le compilateur.

`rustc_driver` annonce que son API est complètement instable.
`rustc_public` vise une future publication semver, mais reste non publié et
sujet à des changements cassants. Aucune de ces surfaces ne constitue
aujourd'hui une base acceptable pour le cœur stable de Temper.

Références :

- `src/tools/compiletest/src/lib.rs:99-140`
- `tests/run-make/README.md:1-23`
- `src/bootstrap/src/core/build_steps/perf.rs:68-132`
- `compiler/rustc_driver_impl/src/lib.rs:1-6`
- `compiler/rustc_public/src/lib.rs:1-16,40-43`

## Conséquences directes pour Temper

### Choix validés

| Choix Temper v0.0.1 | Validation dans Cargo ou Rust |
|---|---|
| `cargo metadata --format-version 1` | Bonne identité workspace/package/target sans importer Cargo |
| `--message-format=json` et `compiler-artifact` | Source autoritative de l'exécutable, indépendante du layout |
| Un target directory par stratégie | Isole fingerprints, outputs, incremental et locks |
| LTO via `profile.release.*` | Laisse Cargo propager correctement objets et bitcode dans le graphe |
| `--target x86_64-unknown-linux-gnu` | Isole les flags target des build scripts et proc macros |
| Chemins PGO absolus et répertoire raw par run | Conforme au workflow rustc et évite les profils obsolètes |
| `llvm-profdata` adjacent au rustc actif | Réduit le risque de mismatch de version LLVM |
| Plusieurs `.profraw` acceptés puis fusionnés | Conforme au comportement documenté de l'instrumentation |
| Pas d'intégration `cargo` ou `rustc_driver` | Évite deux API explicitement instables |

### Findings à traiter

#### F1, visibilité des profils PGO incompatibles

Priorité : haute.

`src/strategy.rs:403-422` injecte `-Cprofile-use`, mais pas
`-Cllvm-args=-pgo-warn-missing-function`. Rust le recommande et le test PGO de
Cargo le qualifie d'essentiel. Sans lui, LLVM peut ignorer silencieusement des
fonctions absentes du profil. Un build PGO réussi ne prouve donc pas que toutes
les données attendues ont été appliquées.

Exploration ciblée requise : déterminer la politique Temper devant ces warnings,
les capturer dans le flux `compiler-message`, puis choisir entre rejet PGO
fail-closed et simple diagnostic persistant.

#### F2, rustflags target sous forme de chaîne

Priorité : haute.

`effective_target_rustflags` accepte les deux formes Cargo :

```toml
rustflags = "-Ctarget-cpu=native"
```

et :

```toml
rustflags = ["-Ctarget-cpu=native"]
```

Mais Temper injecte toujours son override PGO sous forme de liste JSON dans
`src/strategy.rs:128-139`. Le merge Cargo refuse le mélange entre un
`ConfigValue::String` existant et un `ConfigValue::List` CLI
(`src/context/config_value.rs:200-216`). Le test d'intégration de Temper couvre
uniquement la forme liste (`tests/ep003_strategy.rs:236-283`).

Conclusion source-level : la forme chaîne passe le preflight Temper puis risque
d'échouer au build d'instrumentation. Ce cas doit être reproduit et corrigé avant
d'élargir le dogfooding PGO.

#### F3, parité exacte des builds PGO

Priorité : haute.

Rust exige la même configuration rustc entre build instrumenté et build
optimisé. Temper réutilise le même `base_strategy` et les mêmes sources Cargo,
ce qui est sain. Il faut néanmoins établir par test que profils, features,
target, rustflags préexistants, wrappers autorisés et configuration injectée
restent identiques aux seuls changements PGO près. F1 doit rendre toute
divergence observable.

#### F4, contrat exact de `compiler-artifact`

Priorité : moyenne.

Temper exige exactement un exécutable correspondant au package ID, nom de
target et type bin. Cette règle est conservatrice. Une exploration ciblée doit
tester les événements `fresh`, les workspaces avec plusieurs profils ou targets
homonymes, les build scripts, les proc macros et les futurs changements de
schéma tolérés par `cargo_metadata`.

#### F5, observabilité de compilation

Priorité : moyenne, seulement si elle devient un objectif produit.

Cargo fournit `--timings` stable pour un rapport HTML global. Les sections fines
de rustc et les événements par phase reposent encore sur
`-Zsection-timings`, `-Zjson=timings` ou `-Zself-profile`. Temper mesure
actuellement la durée totale du processus Cargo, ce qui est cohérent avec son
objectif runtime v0.0.1. Une attribution query/codegen/link introduirait une
surface nightly distincte, à isoler explicitement du chemin stable.

## Programme d'explorations ciblées

### Vague 1 : durcir le chemin actuel

1. Reproduire F2 avec les deux syntaxes rustflags et documenter exactement le
   merge Cargo CLI/fichier.
2. Tracer les commandes rustc effectives des deux phases PGO et prouver leur
   parité.
3. Étudier le diagnostic `pgo-warn-missing-function` de bout en bout, y compris
   son format JSON et sa politique de rejet.
4. Tester le contrat `compiler-artifact` avec fresh builds, workspaces complexes,
   proc macros et build scripts.
5. Cartographier les inputs de fingerprint affectés par chaque stratégie Temper.
6. Vérifier les locks de target directory et les garanties de concurrence entre
   runs.

### Vague 2 : corpus et observabilité

7. Étudier rustc-perf : format des scénarios, révisions, modes Check/Debug/Opt,
   incrémental et stockage des résultats.
8. Comparer le modèle de capture Cargo avec les besoins d'un corpus Temper
   versionné, licencié, exécutable et muni d'oracles de correction.
9. Étudier `--timings`, self-profile, measureme, time-trace LLVM et les trous de
   mesure du linker.
10. Cartographier le scheduling des codegen units, le jobserver Cargo et les
    événements qui permettraient d'expliquer une régression.

### Vague 3 : options stratégiques, uniquement sur besoin établi

11. Évaluer un wrapper rustc externe pour l'observation sans importer Cargo.
12. Réévaluer `cargo::Executor`, `rustc_public` ou `rustc_driver` seulement si
    une information indispensable ne peut pas être obtenue par processus.
13. Si une surface manque réellement, envisager une contribution stable upstream
    à Cargo avant un fork ou une dépendance aux internals.
14. Étudier les backends alternatifs et `.rlink` uniquement si Temper ajoute un
    objectif de temps de compilation ou de backend selection.

## Décision architecturale provisoire

Temper doit rester un orchestrateur externe pour son prochain incrément :

- processus Cargo stable ;
- métadonnées v1 ;
- messages JSON ;
- configuration documentée ;
- artefacts et profils isolés ;
- aucune dépendance aux structures `Unit`, queries rustc ou formats internes.

La première suite de développement ne devrait donc pas être une intégration plus
profonde à Rust ou Cargo. Elle devrait fermer F1 à F3, établir un corpus
reproductible, puis décider quel manque d'observabilité justifie éventuellement
une surface nightly séparée.

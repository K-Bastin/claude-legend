# Contribuer à Claude Legend

## Branches

| Branche | Rôle | Créée depuis | Fusionnée dans |
| --- | --- | --- | --- |
| `main` | Versions publiées uniquement. Chaque commit de `main` correspond à une release. | — | — |
| `develop` | Intégration continue de la prochaine version. | `main` | `main` (via `release/*`) |
| `feature/<sujet>` | Nouvelle fonctionnalité. | `develop` | `develop` |
| `fix/<sujet>` | Correction de bug. | `develop` | `develop` |
| `chore/…`, `docs/…`, `refactor/…`, `test/…`, `ci/…`, `perf/…` | Maintenance, documentation, outillage. | `develop` | `develop` |
| `release/<x.y.z>` | Préparation d'une version (numéro, derniers correctifs). | `develop` | `main`, puis `develop` |
| `hotfix/<sujet>` | Correction urgente d'une version publiée. | `main` | `main`, puis `develop` |

Noms de branches en minuscules, mots séparés par des tirets : `feature/session-search`, `fix/windows-pty-exit`.

Personne ne pousse directement sur `main` ni sur `develop` : tout passe par une pull request
dont la CI est verte. La CI refuse une PR vers `main` qui ne vient pas de `release/*` ou `hotfix/*`.

## Commits : Conventional Commits

Format : `type(scope): description`, en anglais, à l'impératif, sans majuscule ni point final.

```
feat(sync): detect remote sessions for unmapped projects
fix(pty): flush remaining output before exit event on Windows
docs: document release procedure
```

- **Types** : `feat`, `fix`, `perf`, `refactor`, `docs`, `style`, `test`, `build`, `ci`, `chore`, `revert`.
- **Scopes courants** : `pty`, `sync`, `sessions`, `ui`, `config`, `ci`, `deps`, `release`.
- **Changement incompatible** : `feat(sync)!: …` et/ou un pied de page `BREAKING CHANGE: …`.

Un hook git (husky + commitlint) vérifie chaque message localement, et la CI vérifie les commits et le titre de chaque PR.
Les PR sont fusionnées en *squash* : le titre de la PR devient le message du commit sur `develop`.

## Développement

```sh
npm install            # installe aussi les hooks git
npm run tauri dev      # lancer l'application
npm run typecheck      # vérification TypeScript
npm test               # tests Rust
```

> **Ne travaille pas avec Claude dans l'instance lancée par `npm run tauri dev`.**
> `tauri dev` recompile et relance l'application à chaque modification de `src-tauri/`,
> ce qui coupe les sessions ouvertes (elles sont rouvertes au redémarrage, mais la commande
> en cours est perdue). Pour tes sessions de travail, utilise la version installée, ou
> `npm run dev:no-watch`, qui ne relance pas l'application : ferme-la et relance-la toi-même
> pour prendre en compte une modification Rust.

Le hook `pre-commit` lance `npm run typecheck` et `cargo fmt --check`.
Avant d'ouvrir une PR, vérifie aussi `cargo clippy --all-targets -- -D warnings` dans `src-tauri/`.

## Publier une version

1. Depuis `develop` à jour : `git switch -c release/0.2.0`
2. `npm run version:set -- 0.2.0`, puis `cargo check` dans `src-tauri/` pour mettre à jour `Cargo.lock`.
3. Commit `chore(release): 0.2.0`, push, puis PR `release/0.2.0` → `main`.
4. Après fusion (en *merge commit*, pas en squash) :
   ```sh
   git switch main && git pull
   git tag -a v0.2.0 -m "v0.2.0"
   git push origin v0.2.0
   ```
5. Le workflow `release` vérifie que le tag est sur `main` et correspond à la version du projet,
   compile Linux et Windows, puis crée un brouillon de release avec les notes générées.
   Relis-le et publie-le depuis GitHub.
6. Reporte `main` dans `develop` : PR `main` → `develop`, ou `git switch develop && git merge main`.

Un **hotfix** suit le même chemin depuis `main` : `hotfix/<sujet>`, version patch, PR vers `main`, tag, puis report dans `develop`.

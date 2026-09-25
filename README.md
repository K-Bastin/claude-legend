# Claude Legend

Application desktop (Linux / Windows) qui lance le vrai Claude Code dans un terminal intégré
et synchronise les conversations entre tes PC via un dossier partagé (Syncthing, Nextcloud, OneDrive…).

## Fonctionnement

- Chaque onglet exécute `claude` (le CLI officiel) dans un pseudo-terminal : toutes les
  fonctionnalités (slash commands, MCP, hooks, skills, permissions, /rewind…) sont identiques.
- La barre latérale liste les conversations de `~/.claude/projects`, groupées par projet.
  Un clic lance `claude --resume <id>` dans le bon dossier.
- La synchro copie sessions, sous-agents, mémoire de projet et checkpoints de fichiers
  dans `<dossier partagé>/claude-legend/`, en remplaçant les chemins propres à chaque PC
  (`/home/kb/...` ↔ `C:\Users\kb\...`) par des marqueurs.
- Un projet est reconnu d'un PC à l'autre par son remote git (sinon par le nom du dossier).
  Si un projet n'est pas encore associé sur un PC, l'app demande où il se trouve.
- Une conversation ouverte pose un verrou : les autres PC préviennent avant de l'ouvrir.
  En cas de modifications divergentes, la plus récente gagne et l'autre est sauvegardée
  dans le dossier `conflicts/` des données de l'app.

Le code des projets n'est pas synchronisé : utilise git pour ça.

## Raccourcis

| Touche | Action |
| --- | --- |
| Ctrl+Shift+T | Nouvelle session |
| Ctrl+Shift+W | Fermer l'onglet |
| Ctrl+Tab | Onglet suivant |
| Shift+Entrée | Retour à la ligne dans le prompt |
| Ctrl+C / Ctrl+Shift+C | Copier la sélection (Ctrl+C sans sélection = interrompre) |
| Ctrl+V / Ctrl+Shift+V | Coller (une image du presse-papier est transmise à Claude) |
| Ctrl + / - / 0 | Taille du texte |

Glisser-déposer un fichier dans le terminal insère son chemin.

## Développement

```sh
npm install
npm run tauri dev      # lancer en dev
npm run tauri build    # paquets .deb / .rpm / AppImage (Linux)
npm test               # tests Rust
```

Branches, conventions de commit et procédure de release : voir [CONTRIBUTING.md](CONTRIBUTING.md).
Les paquets Windows (.msi / .exe) sont produits par le workflow `release` lors d'un tag `vX.Y.Z` sur `main`.

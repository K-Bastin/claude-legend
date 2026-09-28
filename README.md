# Claude Legend

Application desktop (Linux / Windows) qui lance le vrai Claude Code dans un terminal intégré
et synchronise les conversations entre tes PC : dossier partagé (Syncthing, Nextcloud, OneDrive…),
serveur SFTP, FTP/FTPS ou WebDAV.

## Fonctionnement

- Chaque onglet exécute `claude` (le CLI officiel) dans un pseudo-terminal : toutes les
  fonctionnalités (slash commands, MCP, hooks, skills, permissions, /rewind…) sont identiques.
- La barre latérale liste les conversations de `~/.claude/projects`, groupées par projet.
  Un clic lance `claude --resume <id>` dans le bon dossier.
- La synchro copie sessions, sous-agents, mémoire de projet et checkpoints de fichiers
  vers la destination choisie, en remplaçant les chemins propres à chaque PC
  (`/home/kb/...` ↔ `C:\Users\kb\...`) par des marqueurs.
- Un projet est reconnu d'un PC à l'autre par son remote git (sinon par le nom du dossier).
  Si un projet n'est pas encore associé sur un PC, l'app demande où il se trouve.
- Une conversation ouverte pose un verrou : les autres PC préviennent avant de l'ouvrir.
  En cas de modifications divergentes, la plus récente gagne et l'autre est sauvegardée
  dans le dossier `conflicts/` des données de l'app.

Le code des projets n'est pas synchronisé : utilise git pour ça.

### Destinations de synchronisation

| Type | Exemple | Remarques |
| --- | --- | --- |
| Dossier synchronisé | `~/Sync` | Synchronisé par un autre outil (Syncthing, client Nextcloud, OneDrive…). Les données sont dans `<dossier>/claude-legend/`. |
| SFTP | `nas.local:22`, dossier `claude-legend` | Mot de passe, clé privée ou agent SSH. L'empreinte du serveur est affichée à la première connexion et vérifiée ensuite. |
| FTP / FTPS | `ftp.exemple.fr:21` | Cocher « FTPS » pour chiffrer (AUTH TLS, certificats du système). |
| WebDAV | `https://cloud.exemple.fr/remote.php/dav/files/moi/claude-legend` | Nextcloud, ownCloud, NAS Synology/QNAP… Utilise un mot de passe d'application si le compte a la double authentification. |

Les mots de passe sont conservés dans le trousseau du système (Secret Service sous Linux,
Gestionnaire d'identifiants sous Windows), jamais dans les fichiers de réglages.
« Tester la connexion » vérifie l'accès en lecture et en écriture.

## Raccourcis

| Touche | Action |
| --- | --- |
| Ctrl+Shift+T | Nouvelle session |
| Ctrl+Shift+W | Fermer l'onglet |
| Ctrl+Tab | Onglet suivant |
| Ctrl+Alt+1…4 | Aller au panneau 1 à 4 (écran partagé) |
| Shift+Entrée | Retour à la ligne dans le prompt |
| Ctrl+C / Ctrl+Shift+C | Copier la sélection (Ctrl+C sans sélection = interrompre) |
| Ctrl+V / Ctrl+Shift+V | Coller (une image du presse-papier est transmise à Claude) |
| Ctrl + / - / 0 | Taille du texte |

Glisser-déposer un fichier dans le terminal insère son chemin.

### Écran partagé

Les boutons à droite des onglets affichent 1, 2 (côte à côte ou empilés), 3 ou 4 conversations
en même temps. La conversation choisie (liste ou onglet) s'ouvre dans le panneau actif, surligné ;
un onglet ou une conversation de la liste peut aussi être glissé directement dans un panneau.
La disposition et le contenu des panneaux sont retrouvés au prochain lancement.

## Développement

```sh
npm install
npm run tauri dev      # lancer en dev
npm run tauri build    # paquets .deb / .rpm / AppImage (Linux)
npm test               # tests Rust
```

Branches, conventions de commit et procédure de release : voir [CONTRIBUTING.md](CONTRIBUTING.md).
Les paquets Windows (.msi / .exe) sont produits par le workflow `release` lors d'un tag `vX.Y.Z` sur `main`.

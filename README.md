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
| FTP / FTPS | `ftp.exemple.fr:21` | FTPS (AUTH TLS, certificats du système) coché par défaut ; sans lui, mot de passe et conversations circulent en clair. |
| WebDAV | `https://cloud.exemple.fr/remote.php/dav/files/moi/claude-legend` | Nextcloud, ownCloud, NAS Synology/QNAP… Utilise un mot de passe d'application si le compte a la double authentification. |

Les mots de passe sont conservés dans le trousseau du système (Secret Service sous Linux,
Gestionnaire d'identifiants sous Windows), jamais dans les fichiers de réglages.
« Tester la connexion » vérifie l'accès en lecture et en écriture.

### Chiffrement de bout en bout

Dans ⚙ → Synchronisation, « Chiffrement de bout en bout » chiffre les conversations sur ton PC
avant l'envoi (XChaCha20-Poly1305, clé dérivée de ta phrase de passe par Argon2id) : la
destination ne peut plus les lire. Avant de l'activer, mets tous tes PC à jour ; saisis ensuite la
même phrase de passe sur chacun (« Déverrouiller »). Un PC sans la phrase de passe arrête de
synchroniser plutôt que d'envoyer quoi que ce soit en clair. La phrase de passe est gardée dans le
trousseau de chaque PC et n'est récupérable nulle part : sans elle, les données chiffrées sont
perdues. Les noms des fichiers et des dossiers (clés de projet, identifiants de sessions) restent
visibles.

### Archives, conflits, recherche, notifications

- **Archiver** une conversation (icône au survol) la masque sur tous les PC sans rien supprimer ;
  « Archives » en bas de la liste permet de la restaurer.
- Quand une conversation a changé sur deux PC à la fois, la version écartée est listée dans
  « version(s) mise(s) de côté » en bas à gauche et peut être restaurée comme conversation à part.
- La recherche porte aussi sur le contenu des conversations de ce PC (à partir de 3 caractères,
  sans tenir compte des accents), avec un extrait.
- Une notification de bureau prévient quand Claude attend ta réponse ou a terminé dans une
  conversation que tu ne regardes pas, et quand le quota dépasse 90 % (désactivable dans ⚙). Elle
  passe par des hooks `Notification` et `Stop` ajoutés aux sessions lancées par l'application ;
  tes propres hooks continuent de s'exécuter.

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

### Règles par projet

L'icône « document » dans l'en-tête d'un projet ouvre ses règles : des instructions ajoutées à
Claude pour toutes les conversations du projet (`claude --append-system-prompt-file`), sans
`CLAUDE.md` ni aucun fichier dans le projet ou dans git. Elles sont partagées via la
synchronisation avec tes autres PC — et ton équipe si elle utilise la même destination — et
prises en compte au prochain lancement ou à la prochaine reprise d'une conversation.

### Quota

En bas à gauche, deux barres montrent l'utilisation de ton forfait Claude : la fenêtre de 5 heures
et la semaine, avec l'heure de réinitialisation. Les valeurs viennent de Claude Code lui-même
(données de sa barre d'état), mises à jour à chaque réponse de Claude dans l'application. Pour cela
l'application se place comme barre d'état des sessions qu'elle lance (`--settings`, sans modifier
tes fichiers de réglages) et relaie ta propre barre d'état si tu en as une.

### Mises à jour

Au lancement, l'application vérifie la dernière release publiée sur GitHub et propose de l'installer
(désactivable dans ⚙ → Mises à jour, qui permet aussi de vérifier à la demande).
L'installation se fait dans l'application, qui redémarre ensuite en rouvrant les conversations
ouvertes ; les mises à jour sont signées et leur signature est vérifiée avant installation.
Installée par paquet (.rpm / .deb), le mot de passe administrateur est demandé pour installer le
nouveau paquet.

### Thème clair / sombre

Le bouton ☀/☾ de la barre latérale bascule entre clair et sombre ; les réglages proposent aussi
« Comme le système ». Claude Code choisit lui-même une partie de ses couleurs : en thème clair,
passe-le aussi en clair avec `/config` → *Theme*.

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

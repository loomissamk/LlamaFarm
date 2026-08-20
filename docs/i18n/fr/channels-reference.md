# Référence des canaux (Français)

Cette page est une localisation initiale Wave 1 pour les capacités de canaux et les chemins de configuration.

Source anglaise:

- [../../channels-reference.md](../../channels-reference.md)

## Quand l'utiliser

- Comparer les capacités Telegram/Discord/Slack/etc.
- Vérifier les règles allowlist et frontières de sécurité
- Diagnostiquer les problèmes d'entrée/sortie messages

## Règle

- Les identifiants de canaux, API paths et config keys restent en anglais.
- La définition finale est la source anglaise.

## Configuration Discord dans le dashboard

Ouvrez **Connections -> Discord -> Connect Discord**, puis saisissez le bot token,
un guild ID facultatif et au moins un user ID numérique autorisé. LlamaFarm
vérifie et affiche le bot appairé, puis enregistre `[channels_config.discord]`
sans renvoyer le secret au navigateur. **Add to server** ouvre l'installation
OAuth officielle de Discord ; Update et Disconnect fonctionnent ensuite comme
pour GitHub. Activez **Message Content Intent** dans le portail Discord, puis
redémarrez le nœud une fois pour démarrer, mettre à jour ou arrêter le listener.

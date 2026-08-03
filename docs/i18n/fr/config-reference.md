# Référence de configuration (Français)

Cette page est une localisation initiale Wave 1 pour les clés de configuration et les valeurs par défaut.

Source anglaise:

- [../../config-reference.md](../../config-reference.md)

## Quand l'utiliser

- Initialiser un nouvel environnement
- Vérifier les conflits de configuration
- Auditer les paramètres de sécurité/stabilité

## Règle

- Les noms de clés de configuration restent en anglais.
- Le comportement runtime exact est défini en anglais.

## Notes de mise à jour

- Ajout de `provider.reasoning_level` (OpenAI Codex `/responses`). Voir la source anglaise pour les détails.
- La valeur par défaut de `agent.max_tool_iterations` est `0` (illimité) : l'exécution continue jusqu'à la fin, un blocage/une erreur réelle ou une annulation explicite par l'opérateur. Une valeur positive définit toujours une limite explicite par tour.
- La valeur par défaut de `research.max_iterations` est également `0` (illimité) : une recherche productive continue jusqu'à sa fin, une erreur provider/tool ou le déclenchement du détecteur d'appels/résultats identiques. Une valeur positive fixe une limite de recherche explicite.
- La valeur par défaut de `host_runner.max_exec_timeout_secs` est `0` (aucun maximum) : un `timeout_secs` absent ou nul n'impose pas de délai mural, tandis qu'une valeur positive fixe un délai explicite.
- `scheduler.max_concurrent` vaut `4` par défaut et limite le nombre total de tâches planifiées simultanées entre tous les cycles de polling ; `0` signifie illimité. `LLAMAFARM_SCHEDULER_MAX_CONCURRENT` remplace cette valeur.
- `gateway.require_pairing` est un ancien champ de compatibilité dont la valeur par défaut est `false`; le pairing est retiré et cette valeur est ignorée à l'exécution.
- Ajout de `model_routes[].api_url` pour remplacer `api_url` route par route. Utile pour cibler plusieurs endpoints locaux séparés du même type de provider.
- `provider.ollama_num_ctx` est une valeur manuelle exacte ; le tableau de bord
  accepte 2 048–262 144. Sans cette valeur, `OLLAMA_NUM_CTX` fournit la valeur
  runtime par défaut.
- Avec `LLAMAFARM_ADAPTIVE_CONTEXT=true`, la valeur d'environnement devient la
  base rapide. LlamaFarm agrandit la fenêtre par paliers ×2 selon le besoin,
  jusqu'au minimum entre la longueur native du modèle et
  `LLAMAFARM_ADAPTIVE_CONTEXT_MAX` (262 144 par défaut).
- `provider.ollama_workers` définit des processus Ollama liés à un GPU ou à un
  groupe de GPU ; `provider.ollama_model_placements` leur affecte les modèles.
  `spread = true` répartit un modèle sur le groupe, tandis que des workers
  séparés permettent de garder plusieurs modèles résidents simultanément.
- Les connexions nommées `[[db_connections]]` alimentent Database Explorer et
  les outils `db_schema`/`db_query`. Les drivers pris en charge sont `sqlite`,
  `postgres`, `mysql` (y compris MariaDB) et `mongodb`. MySQL/MariaDB nécessite
  le feature Cargo `db-mysql`, activé par défaut dans l'image Docker groupée.

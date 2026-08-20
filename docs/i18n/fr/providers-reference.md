# Référence des providers (Français)

Cette page est une localisation initiale Wave 1 pour vérifier les IDs provider, alias et variables d'authentification.

Source anglaise:

- [../../providers-reference.md](../../providers-reference.md)

## Quand l'utiliser

- Choisir un provider et un modèle
- Vérifier ID/alias/env vars de credentials
- Diagnostiquer les erreurs de configuration/auth

## Règle

- Les IDs provider et noms d'env vars restent en anglais.
- La source normative de comportement est l'anglais.

## Notes de mise à jour

- Ajout d'un réglage `provider.reasoning_level` pour le niveau de raisonnement OpenAI Codex. Voir la source anglaise pour les détails.

## Contexte adaptatif Ollama

- `provider.ollama_num_ctx` est une valeur manuelle exacte.
- Sans cette valeur, Auto utilise le maximum natif déclaré par le modèle.
- `OLLAMA_NUM_CTX` ne devient une base rapide que lorsque
  `LLAMAFARM_ADAPTIVE_CONTEXT=true`; il ne limite pas le mode Auto ordinaire. La croissance
  est limitée par le minimum entre la longueur native du modèle et
  `LLAMAFARM_ADAPTIVE_CONTEXT_MAX`.
- Vérifiez le contexte réellement alloué par Ollama avec
  `docker exec LlamaFarm ollama ps`.

# Passerelle de localisation : CI Workflow Map

Cette page est une passerelle enrichie vers la spécification CI anglaise.
Les seuls workflows GitHub Actions exécutables actuels sont `ci-run.yml` et
`docs-deploy.yml`.

Source anglaise :

- [../../ci-map.md](../../ci-map.md)

## Positionnement du sujet

- Catégorie : processus d'ingénierie et livraison
- Profondeur : passerelle enrichie (sections + conseils d'exécution)
- Usage : comprendre la structure puis appliquer la source anglaise

## Plan des sections source

- [Executable Workflow Baseline](../../ci-map.md#executable-workflow-baseline)
- [Core CI Contract](../../ci-map.md#core-ci-contract)
- [Docs Pages Contract](../../ci-map.md#docs-pages-contract)
- [Local Reproduction](../../ci-map.md#local-reproduction)
- [Fast Triage](../../ci-map.md#fast-triage)
- [Maintenance Rules](../../ci-map.md#maintenance-rules)

## Conseils d'exécution

- Le nom stable du contrôle de fusion est `CI Required Gate`.
- Une PR de documentation construit seulement le site ; seul `main` déploie
  GitHub Pages.
- Les commandes, clés de configuration, chemins API et identifiants de code
  restent en anglais.
- En cas d'ambiguïté, la source anglaise fait foi.

## Entrées liées

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)

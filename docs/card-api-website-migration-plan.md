# Card API × alteredcore-website migration — plan et journal de décisions

**Status:** living document, à reprendre. Rien n'est encore implémenté (voir "Prochaines actions").

Contexte : remplacer partout l'usage de `cards.alteredcore.org` dans
`C:\Users\Reverb\Documents\repos\alteredcore-website` par cette API de recherche
(`uniques-search-api`). Chantier découpé en 7 priorités par l'utilisateur ; ce document couvre en
détail la phase 1 (déjà cadrée) et rappelle les phases suivantes pour mémoire.

## Roadmap complète, dans l'ordre de priorité donné

1. **Contenu de la réponse par carte** — ce que l'API doit renvoyer pour qu'un client (le renderer
   en particulier) n'ait plus besoin d'appeler `cards.alteredcore.org/api/cards/batch`. Implique de
   revoir tous les contextes d'usage sur le site, ajouter ce qui manque, retirer le mort, déduire
   plutôt que stocker quand possible. **→ cadrage fait, voir plus bas ; implémentation pas commencée.**
2. Refondre la recherche d'**uniques** sur alteredcore-website pour utiliser la nouvelle API, adapter
   les filtres de l'interface aux nouvelles fonctionnalités, donner au renderer les infos en direct
   (pas de round-trip supplémentaire).
3. **Déployer** ce premier niveau — probablement stocker la base de cartes (artefacts d'index) sur
   Scaleway, et automatiser un minimum leur production, pour pouvoir livrer au fur et à mesure.
4. Alléger l'**API ownership** de sa logique de recherche (uniques possédées, alt-arts, filtres de
   collection) en la déléguant à cette API : transmettre la recherche avec un id de collection
   contenant l'id de version de la collection du joueur (ex. id du dernier event) ; créer un id de
   collection dédié pour la liste des alt-arts de base dédupliquées ; si la collection n'existe pas
   côté API de recherche, la créer puis relancer la recherche.
5. Faire la même chose côté **recherche principale** (`cards.alteredcore.org` / "all cards").
6. **Fusionner** les recherches digitale / all cards / uniques pour permettre des recherches
   croisées.
7. Remplacer les **autres endroits** de l'appli où la recherche est utilisée.

## Findings — phase 1 (contenu de la réponse par carte)

### Le renderer supporte déjà l'injection de données — découverte clé
`altered-card-renderer-minified.js` (vendorisé dans
[`demo-ui/src/altered-card-renderer-minified.js`](../demo-ui/src/altered-card-renderer-minified.js),
et chargé en CDN **non versionné** (`@main`) depuis `PolluxTroy0/Altered-Card-Renderer` sur
alteredcore-website) est **la même librairie** dans les deux cas, avec deux modes d'usage :

- `<altered-card ref locale>` (custom element déclaratif, utilisé aujourd'hui partout sur le site) :
  auto-fetch par défaut via `cardApiUrl` (template pointant sur `cards.alteredcore.org`), overridable
  (`embeddedConfig.cardApiUrl`, attribut `data-proxy`).
- `window.AlteredRender.mountFromApi(el, json, fieldMap?)` (impératif, déjà utilisé par demo-ui,
  voir [`AlteredCardSlot.tsx`](../demo-ui/src/components/AlteredCardSlot.tsx) /
  [`cardToAlteredApiJson.ts`](../demo-ui/src/api/cardToAlteredApiJson.ts)) : zéro fetch caché, on lui
  donne le JSON directement.

**Décision retenue** : migrer tous les points d'affichage du site vers `mountFromApi` avec les
données déjà en main depuis la recherche — élimine `/api/cards/batch` complètement (et l'auto-fetch
caché du custom element), pas juste un contournement de config.

### Champs réellement lus par le renderer (tous types de carte)
Désobfusqué depuis le minifié : `reference`, `name`, `forge.lang`/`forge.collection`,
`cardRarity.reference` (fallback `rarity.reference` puis `cardGroup.rarity.reference`),
`cardType.reference` (choix du gabarit : TOKEN/HERO/PERMANENT/EXPEDITION_PERMANENT/SPELL/
CHARACTER...), `cardSubTypes[].reference`, `set.reference`/`set.code`,
`mainCost`/`recallCost`/3 powers, `mainEffect`/`echoEffect` (présence + longueur du texte influence
le gabarit), `artists[0].name`, `collectorNumberFormatedId`. Comme `mountFromApi` accepte un
`fieldMap`, pas obligé de matcher ces noms exacts.

### `/api/cards/batch` sur alteredcore-website — 2 appelants seulement
Champs réellement lus en aval, tous appelants confondus : `reference, name, faction.code,
rarity.reference, set.reference`. Rien d'autre n'est jamais lu (favoris, noms de decks tournoi).
`CardV2` couvre déjà largement ce besoin.

### Écarts identifiés sur `CardV2` (branche unique)
- `rarity`/`cardType` absents de `CardV2` mais triviaux à déduire pour une unique (toujours
  `UNIQUE`/`CHARACTER`) — à renvoyer explicitement plutôt que de laisser chaque client réinventer
  l'hypothèse (demo-ui le hardcode déjà côté client aujourd'hui).
- `isBanned`/`isErrated`/`isSuspended` : déjà parsés dans `CardJson` et indexés (bitmaps
  `status_index`, D16 dans
  [`docs/non-unique-refonte-decisions.md`](non-unique-refonte-decisions.md)) mais **jamais
  sérialisés** dans `CardV2` (voir
  [`uniques-http-api/src/http/api/cards/models.rs`](../uniques-http-api/src/http/api/cards/models.rs)).
- `cardRulings` : **aucune donnée nulle part dans CardsData**, vérifié directement sur le checkout
  local `C:\Users\Reverb\Documents\GitHub\CardsData` (aucun CSV, aucune colonne). Reste hors scope
  tant qu'aucune source n'apparaît ; dégrade déjà gracieusement aujourd'hui côté site
  (`adaptUniqueCard()` laisse le champ absent, le rendu le tolère).
- `loreEntries` : donnée réelle disponible — `data/csv/LoreStories.csv` (1038 lignes). Structure
  vérifiée (PowerShell `Import-Csv`, pas un simple split sur virgule à cause des champs entre
  guillemets) :
  - **un-à-plusieurs par famille** (jusqu'à 9 entrées pour une même `CardFamilyId`) → un tableau,
    pas un objet.
  - texte **en/fr uniquement** (`FlavorText_en/fr`, `Inspiration_en/fr`, `Story_en/fr`, `Narrator`,
    `Name`, `Date`) — pas le pattern 5-locales (en_US/fr_FR/de_DE/es_ES/it_IT) utilisé partout
    ailleurs dans l'index.
  - 985/1038 lignes liées via `CardFamilyId` directement ; 53 lignes restantes n'ont qu'un
    `UnresolvedCardReference` au format non standard (ne matche pas
    `index_core::path::parse_card_reference`) — à ignorer silencieusement en v1, même esprit que le
    skip déjà fait pour les lignes `FOILER` non parseables (D9).

### Légalité de formats ("legal in / not legal in")
Site actuel (`plugins/core-altered-cards/pages/card.php` sur alteredcore-website) : deux mécanismes
distincts.
- **Formats "statiques"** (la majorité) : calculés côté client depuis `isBanned`/`isSuspended`/
  rareté/`isUnique` (`isFormatStaticLegal()`). **Bug latent déjà présent aujourd'hui** :
  `adaptUniqueCard()` laisse ces champs `undefined` quand `UNIQUES_API` est configuré → tout format
  statique affiche silencieusement la carte comme légale, bannie/suspendue ou non. Indépendant de
  cette migration, mais elle le corrige comme effet de bord.
- **Formats "dynamiques"** (allowlist/exclude-list arbitraire, non déductible d'une règle statique) :
  aujourd'hui un round-trip réseau (`GET /api/v2/cards?ref=X&format=frontier`) par carte affichée.
  Seul `frontier` est marqué `requireUniqueLegality` côté config site ; `living-legend`
  ([`formats/living-legend.json`](../formats/living-legend.json), une exclude-list de refs tout
  aussi arbitraire) ne semble pas l'être dans le code lu → légalité probablement affichée à tort
  pour les cartes qu'il devrait exclure. À vérifier côté site (`loadAlteredData('formats')`, pas
  dans ce repo).

**Décision retenue** : ne pas ajouter la légalité à chaque carte de chaque réponse de recherche
(inutile, les grilles n'affichent pas ces chips). Scoper à `GET /api/v2/card/{reference}`
uniquement :
- `isBanned`/`isErrated`/`isSuspended` (répare tous les formats statiques d'un coup).
- `legalFormats: string[]` (ids des formats actuellement chargés où la carte apparaît, calculé via
  appartenance aux bitmaps déjà en mémoire — O(nb formats) pour une seule carte) — remplace le
  round-trip par format et par carte, unifie statique/dynamique derrière un seul mécanisme côté
  serveur (le client n'a plus besoin de dupliquer les règles de légalité en JS).
- Reste unique-only tant que les bitmaps de format ne couvrent pas la branche non-unique
  (limitation D18 déjà connue dans le décision log non-unique).

### Parité non-unique (nécessaire pour l'onglet "all cards" et les alt-arts)
`SearchCard::NonUnique` (`/api/v2/search`, voir
[`uniques-http-api/src/http/api/search.rs`](../uniques-http-api/src/http/api/search.rs)) expose déjà
`reference/faction/rarity/product/serialized/mainCost/recallCost/mountainPower/oceanPower/
forestPower` — mais pas `name/artist/set/cardSubTypes/cardType/collectorNumber/effects/
banned-errated-suspended`, alors que ces jointures existent déjà côté index (D16/D17 : type/subtype/
family/status sont indexés, juste pas retournés). Chantier identique à ce que D18 a déjà fait pour
la branche unique ("flatten a real CardV2") — précédent direct à suivre pour la branche non-unique.

## Principe de design retenu
Endpoint carte unitaire (`GET /api/v2/card/{reference}`) enrichi (formats/status/lore, et rulings à
terme si une source apparaît) ; réponses de liste/recherche (`/api/v2/cards`, `/api/v2/search`)
restent légères. La donnée peut être stockée au niveau index (`catalog.json`, niveau famille) sans
être sérialisée par le chemin de liste — même schéma que `artist`/`card_sub_types` aujourd'hui.

## Prochaines actions (ordre proposé, aucune commencée)
1. **Petit chantier, zéro nouvelle ingestion** : ajouter `isBanned`/`isErrated`/`isSuspended` +
   `legalFormats: string[]` sur `GET /api/v2/card/{reference}` dans `uniques-http-api`. Toute la
   donnée est déjà indexée/chargée en mémoire (status bitmaps + `FormatIndex`).
2. **Chantier plus gros** : ingestion du lore — nouveau lecteur CSV dans
   [`index-core/src/cardsdata.rs`](../index-core/src/cardsdata.rs) (`LoreStories.csv`), nouveau
   champ tableau sur `FamilyMetadata`/`catalog.json` (niveau famille), sérialisé uniquement par
   l'endpoint carte unitaire. Skip silencieux des 53 lignes `UnresolvedCardReference` non résolues.
3. Une fois ces deux chantiers faits (ou avant, si priorité change) : upgrade parité non-unique sur
   `SearchCard::NonUnique` (name/artist/set/subtypes/cardType/collectorNumber/effects/status), suivant
   le précédent D18 côté unique.
4. Ensuite seulement : attaquer la phase 2 (recherche uniques sur alteredcore-website).

## Notes annexes (observations utiles, pas des décisions)
- alteredcore-website charge le renderer depuis un CDN non versionné (`@main`) — risque de rupture
  silencieuse en amont, indépendant de cette migration mais à garder en tête.
- `POST /api/v2/collection/{id}` est en cache mémoire uniquement (mini_moka, TTL/idle configurable
  via `CollectionsSettings`) — pas de stockage durable aujourd'hui. À vérifier si ça convient au
  besoin de la phase 4 ("id de collection = dernier event du joueur") : recréation à la demande
  probablement acceptable (le flux "si absente, créer puis relancer" décrit par l'utilisateur
  suppose déjà cette possibilité d'absence), mais pas encore confirmé pour ce cas d'usage précis.
- alteredcore-website a déjà un chemin `UNIQUES_API_URL` partiellement câblé pour l'onglet uniques
  (`card-search.js`, `card.php`, `deckbuilder.php`) — un template de bascule ancien→nouveau existe
  déjà en dur dans le code (`AlteredCard.uniquesApiBase`, `adaptUniqueCard()`), sert de référence
  directe pour la phase 2.

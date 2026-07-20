# VESCM-200 bounded pipeline evaluation

Suite: `vescm-194-loader-path-v1` · case: `historical-vescpkg-native-loader`

| Operating point | Complete path | Missing-facet answer | FrontierShortcutRate | Model calls (complete/incomplete) |
|---|---:|---:|---:|---:|
| HardRulesOnly | true | false | 0.000 | 1/0 |
| FastPlanner | true | false | 0.000 | 2/1 |
| PlannerAndCritic | true | false | 0.000 | 3/2 |

All operating points answer only the six-facet, five-relationship gold path. The missing-facet adversary returns an insufficiency report without calling the answerer.

# Catálogo de cenários

Todos os cenários usam o modelo 2D do M1. As falhas combinadas reutilizam as mesmas janelas inclusivas, ruído inteiro, perda de pacotes e atrasos lógicos. O M2 não acrescenta física nem um protocolo de comunicação.

## Cenários do M2

| Ficheiro em `scenarios/m2/` | Condições | Evidência a procurar |
| --- | --- | --- |
| `a-dropout-pending-delay.json` | Perda de GPS nos passos 4 a 7 e atrasos de 1 a 3 passos | Pacotes anteriores à perda de GPS continuam pendentes; a idade conserva o instante de amostragem |
| `b-noise-recovery-loss.json` | Perda de GPS, ruído de até 120 mm, duas janelas de perda de pacotes e atrasos de até 2 passos | Receber uma mensagem não basta para recuperar: é necessária confiança suficiente durante o intervalo configurado |
| `c-intermittent.json` | Quatro janelas de perda de GPS, três de perda de pacotes e atrasos limitados | Transições repetidas de paragem e recuperação; ausência de deslocamento durante baixa confiança |
| `d-safe-held-stall.json` | Perda prolongada de GPS e pacotes | Segurança PASS, mas missão estagnada por observações insuficientes |
| `e-incomplete-safe.json` | Missão longa com horizonte de oito passos | Segurança PASS e missão incompleta, ainda com progresso |
| `f-zone-nominal.json` | Trajetória nominal junto de uma zona restrita | Comparação de referência para a trajetória com ruído |
| `g-zone-fault.json` | Mesma geometria e missão, com ruído de GPS | Violação natural de `restricted_zone`, sem alterações deliberadas do controlador |
| `h-bounds-nominal.json` | Missão junto do limite do ambiente | Comparação de referência para erro de localização |
| `i-bounds-fault.json` | Mesma missão, com ruído de GPS | Violação natural de `world_bounds` |
| `j-invalid-transition.json` | Alteração deliberada já usada no M1 | Classificação `invalid_terminated` e deteção de `valid_state_transitions`; não é um exemplo natural |

Os pares nominal/falhado usam um limiar de confiança permissivo para expor as limitações do controlador. A comparação mantém a missão, a geometria e esse limiar: o ruído muda a observação e pode mudar a direção escolhida. O verificador continua ativo e observa a trajetória real.

Uma falha esperada no catálogo é uma demonstração da deteção. Não é uma missão bem-sucedida. O resultado da campanha compara a execução com uma expectativa explícita e apresenta separadamente segurança e progresso.

## Regressões do M1

Os oito cenários originais permanecem em `scenarios/`. `sha256.json` fixa os ficheiros de entrada; `canonical-seed-42.json` fixa os bytes dos eventos e relatórios da semente 42. Estes cenários cobrem missão nominal, perda de GPS, comunicação, entrada e contacto na zona restrita, limites do ambiente e alterações deliberadas para testar os monitores de paragem e transições.

`safe-fallback-failure.json` e `invalid-transition.json` usam `validation_mutants`. Os exemplos naturais do M2 não usam esse mecanismo.

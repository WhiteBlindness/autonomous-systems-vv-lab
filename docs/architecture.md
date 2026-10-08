# Contrato do modelo e da reprodução

## Fronteiras do sistema

O modelo do veículo guarda posição real, orientação e movimento. O sensor gera observações a partir desse estado e da entrada de falhas. A comunicação entrega, atrasa ou perde essas observações. O controlador toma decisões a partir do estado observado e dos pontos de passagem da missão, sem ler a posição real.

O verificador observa o estado real, o estado observado e as transições. Não altera as decisões do controlador. Esta separação permite detetar tanto trajetórias inseguras como decisões baseadas em localização insuficiente. No M1, o contexto interno de execução ainda agrega estes estados. O controlador atual só lê observações para decidir, mas os tipos ainda não impõem essa fronteira.

## Tempo e números

Cada iteração corresponde a um passo fixo. O número do passo é a fonte de tempo da simulação; o tempo lógico é o produto desse número pela duração configurada. A execução não espera pela passagem de tempo real.

| Campo | Valor por omissão | Significado |
| --- | --- | --- |
| `tick_ms` | 100 | Duração lógica de cada passo, em milissegundos |
| `safety.confidence_threshold_permille` | 500 | Confiança mínima, numa escala de 0 a 1 000 |
| `safety.fallback_after_ticks` | 3 | Passos consecutivos abaixo do limiar até entrar em paragem de segurança |
| `safety.recovery_after_ticks` | 2 | Passos consecutivos com confiança suficiente até retomar |

As janelas de falhas incluem os dois extremos: `start_tick` e `end_tick`. Os passos da execução vão de 1 até `steps`, inclusive.

As posições usam milímetros inteiros. A orientação admite quatro direções. O deslocamento por passo é explícito. A chegada a um ponto de passagem usa distância de Manhattan, `|dx| + |dy|`, em vez de distância euclidiana. Um passo inteiro pode ultrapassar um ponto; ajustar o raio de chegada ao passo e à grelha da missão. Não há garantia de convergência para qualquer configuração.

A ordem de atualização faz parte do contrato: entradas, entrega de observações, confiança, decisão, movimento e verificação têm uma sequência definida.

O cenário impõe limites de dimensão, duração, vértices e severidade das falhas. A geometria usa operações inteiras com capacidade suficiente para os limites aceites. O polígono tem de ser simples e ter área positiva; os seus vértices podem ser fornecidos em qualquer dos dois sentidos. A última aresta liga implicitamente o último vértice ao primeiro, sem repetir o vértice inicial na lista.

Tocar na fronteira da zona restrita conta como violação. Verificar o segmento percorrido entre estados evita que um veículo atravesse uma zona estreita sem produzir um ponto de telemetria no interior.

## Missão e segurança

Os estados de missão e segurança são distintos. A missão começa pendente, passa a execução e pode concluir. A segurança pode passar de nominal a paragem de segurança e regressar após recuperação da confiança. A paragem suspende o movimento, mas não apaga a missão.

As observações transportam a posição estimada, instante de amostragem e confiança. Uma observação antiga não se torna recente quando a comunicação a entrega. Mensagens com amostragem anterior à última observação aceite não podem substituir uma localização mais recente.

A confiança base é `1000 - floor(1000 * (|ruído_x| + |ruído_y|) / (2 * noise_max_mm))`; sem ruído, é 1 000. Depois, a confiança efetiva diminui linearmente com a idade: `floor(base * (idade_máxima + 1 - idade) / (idade_máxima + 1))`, até zero para uma amostra expirada. São regras de um sensor simulado, não estimativas calibradas de erro real.

A condição de entrada em segurança usa uma sequência contínua de passos com confiança abaixo do limiar. O verificador mede essa condição independentemente do estado interno do controlador. Os cenários com `validation_mutants` permitem contrariar deliberadamente o controlador e testar a deteção.

| Subsistema | Arestas permitidas |
| --- | --- |
| Missão | `pending` para `running`; `running` para `completed` |
| Segurança | `nominal` para `fallback`; `fallback` para `nominal` |

Manter o mesmo estado não emite uma transição. `completed` é terminal. A falta de uma observação utilizável impede movimento, mesmo antes de o contador atingir o intervalo que exige a transição para `fallback`.

## Falhas e ordem

GPS e comunicação têm configuração independente. O GPS pode perder amostras por janela explícita ou probabilidade por passo e pode acrescentar ruído inteiro limitado. A comunicação pode perder pacotes e aplicar atraso em passos lógicos.

Os sorteios usam um gerador pseudoaleatório com semente explícita. A ordem e o número de sorteios fazem parte do comportamento do formato. Os eventos recebidos no mesmo passo têm desempate estável pelo número de sequência.

O gerador é XorShift64*, com estado de 64 bits e multiplicação com retorno modular explícito. A semente zero usa o estado inicial `0x9e3779b97f4a7c15`. Os testes fixam uma sequência de referência. Este gerador serve a repetibilidade da simulação, não aplicações criptográficas.

## Artefactos

`events.json` contém cenário, semente, parâmetros, entradas ordenadas, hashes e resultados necessários à comparação. `report.json` contém o relatório legível por ferramentas, com transições, telemetria e evidência dos invariantes.

A serialização canónica usa JSON compacto com campos estruturados numa ordem fixa. Não inclui caminhos de saída, carimbos de tempo real ou medições de desempenho. As medições da campanha ficam fora dos resultados canónicos.

A reprodução valida a versão, os limites, a sequência, a integridade e o contrato dos eventos. Reaplica as entradas à simulação e recalcula os resultados. Confirma também que cada entrada coincide com a sequência gerada pela semente, pelo que depende da mesma versão do gerador. Compara o resultado completo, não apenas o estado final. Uma execução que falhou um invariante pode ser reproduzida corretamente.

SHA-256 protege contra alterações acidentais e permite comparar artefactos. Não autentica autoria. Para preservar proveniência contra substituição maliciosa, guardar o hash esperado fora do próprio ficheiro ou usar uma assinatura num marco posterior.

## Evolução

Alterações na ordem de eventos, arredondamentos, gerador, confiança, grafo ou serialização podem mudar o comportamento. Exigem cenários de regressão e decisão explícita sobre a versão do contrato. Separar serviços ou introduzir um protocolo só se houver uma necessidade real de interface.

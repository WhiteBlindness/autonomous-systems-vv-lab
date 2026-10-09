# Resultados do M3

O M3 acrescenta contratos temporais declarativos, um modelo explícito do atuador e evidência ligada aos eventos que permite reproduzir e reduzir uma falha. O motor Rust valida e avalia os contratos. A campanha Python organiza as execuções, confere a repetição e chama o minimizador.

## Exemplo: paragem ignorada pelo atuador

O exemplo usa a missão `scenarios/m3/nominal-stop.json`, o conjunto `contracts-stop.json`, o atuador `actuator-continued-stop.json` e a semente 42. O controlador termina o percurso e pede paragem no passo 7. O atuador continua a aplicar o movimento anterior para Este, 250 mm por passo.

Contrato temporal executado pelo monitor Rust:

```json
{
  "schema_version": 1,
  "contracts": [
    {
      "type": "bounded_response",
      "id": "stop_response",
      "trigger": "stop_commanded",
      "required": "vehicle_stationary",
      "within_ticks": 2
    }
  ]
}
```

Configuração do atuador: semente 42, falha `continued_movement`, identificador `stuck_after_stop`, janela inclusiva dos passos 7 a 9 e probabilidade 1 000 por mil. A condição `stop_commanded` dispara no passo 7. O prazo inclui o passo do gatilho e é calculado como `7 + 2 = 9`. O veículo pode parar até ao fim do passo 9, inclusive.

| Passo | Comando do controlador | Ação realizada | Posição real após movimento | Evidência relevante |
| --- | --- | --- | --- | --- |
| 6 | Este, 250 mm | Este, 250 mm | (2 500, 1 000) mm | Comando de origem, evento 45 |
| 7 | Parar | Este, 250 mm | (2 750, 1 000) mm | Gatilho de paragem, evento 54; atuador continua o comando do evento 45 |
| 8 | Parar | Este, 250 mm | (3 000, 1 000) mm | Continuação do movimento, evento 62 |
| 9 | Parar | Este, 250 mm | (3 250, 1 000) mm | Prazo expirado e primeira violação, evento 70 |
| 10 | Parar | Parado | (3 250, 1 000) mm | O movimento cessa depois do prazo |

O facto observado é que o estado real se deslocou 250 mm para Este no passo 9, apesar do comando de paragem. O monitor registou `FAIL`, `first_trigger_tick: 7`, `deadline_tick: 9`, `first_violation_tick: 9`, condição esperada `vehicle_stationary` e observação `known_false`. Os eventos identificam a falha `stuck_after_stop` e o comando de movimento de origem. A interpretação causal é sustentada pelos eventos `continued_movement` do atuador; o resultado não depende de uma expectativa codificada como resultado da verificação.

A segurança geométrica passou neste exemplo e a missão foi classificada como concluída. Isso não apaga a falha temporal. O código de saída de `run` mantém a semântica legada, baseada nos invariantes, pelo que é `0`; o estado temporal é apresentado e guardado separadamente.

## Redução e reprodução

Comandos usados em Windows PowerShell:

```powershell
cargo build --release
$env:VV_LAB_BINARY = "target/release/vv-lab.exe"
python scripts/m3_minimize_failure.py scenarios/m3/nominal-stop.json --contracts scenarios/m3/contracts-stop.json --actuator scenarios/m3/actuator-continued-stop.json --contract-id stop_response --seed 42 --output runs/m3-stop-counterexample --max-candidates 8 --time-budget-seconds 30
target/release/vv-lab.exe replay runs/m3-stop-counterexample/original/events.json
target/release/vv-lab.exe replay runs/m3-stop-counterexample/reduced/events.json
```

Em Linux, usar `target/release/vv-lab` nos dois comandos de reprodução. A reprodução localiza os artefactos laterais M3 junto a `events.json`, volta a executar a simulação e o monitor e compara os resultados. Em ambos os sistemas, a campanha também repete cada caso e compara os hashes canónicos.

O minimizador reduziu o horizonte de 16 para 9 passos. Conservou `stop_response`, o operador `bounded_response`, o gatilho no passo 7, o prazo no passo 9, a primeira violação no passo 9 e o mecanismo `continuing_actuation`. O monitor escolhe o gatilho da obrigação falhada mesmo quando uma obrigação anterior do mesmo contrato passou. Também representa falhas temporais sem falha do atuador como `no_actuator_fault_observed`. Os eventos original e reduzido foram reproduzidos; ambos mantêm a mesma identidade da falha. A pesquisa verificou um candidato e terminou em 0,1331 s neste computador. É uma redução limitada, não uma garantia de mínimo global.

| Hash SHA-256 | Original | Reduzido |
| --- | --- | --- |
| Identidade canónica `source_artifact_sha256` em `verification.json` | `d9bcdb476f651bff9650762b7cb3375a39d9c060953f9999a61093257cf8cc7e` | `36e276570cc256120abb9fa435eec20357d243581180548198c82aa53e7f2b97` |
| Bytes de `events.json` | `29977dcfbab0c1ade9100a85ba7904ffc36113a96aba5a4b5372790e21f78d2f` | `b6e19190355a22e50a6590557204ab387d3c0da1111b0d92f790ed253be025da` |

O hash canónico é o campo `artifact_sha256` do artefacto de eventos v1 e liga o sidecar ao registo reproduzido. Os hashes de bytes permitem validar os ficheiros concretos apresentados acima. A configuração original tem hash `cbdfdbce7539c419735afa66d2e83d3e44629f60e372fc2350f940d722baa484`; o conjunto de contratos tem hash `121735ca65a08894ae912f72796f3ae3a7195b21ac297910e42da25d6d69720e`; a configuração do atuador tem hash `aa0d31c707bf8feaac3daf1088ec8f9c116188d3b1d4ed6379e109cf8adff165`. Os hashes dos artefactos, incluindo `verification.json`, e os comandos completos ficam em `runs/m3-stop-counterexample/summary.json`.

## Campanha M3

Execução local em Windows, em 08/10/2026, com as sementes 42, 1337 e 2026, dez cenários, limite de 30 casos, 90 invocações, 64 MiB de artefactos e 120 s:

```powershell
python scripts/run_campaign.py --binary target/release/vv-lab.exe --suite m3 --seeds 42,1337,2026 --max-runs 90 --max-cases 30 --max-artifact-bytes 67108864 --time-budget-seconds 120 --output-root runs/m3-campaign
```

Os 30 casos previstos passaram as expectativas. A campanha fez 30 repetições determinísticas e 30 reproduções, sem falhas inesperadas, em 3,1714 s. Os resultados agregados foram:

| Dimensão | Resultado |
| --- | --- |
| Contratos temporais | 9 `PASS`, 18 `FAIL` esperados e 3 `INCONCLUSIVE` esperados |
| Segurança | 21 `PASS` e 9 casos com violações esperadas |
| Progresso da missão | 15 concluídas e 15 incompletas |
| Falhas temporais por contrato | `command_applied`: 3; `command_resolution`: 3; `fallback_response`: 3; `mission_progress`: 3; `no_motion_in_fallback`: 6; `restricted_zone_clear`: 3; `stop_response`: 3 |

O catálogo inclui controlo nominal, degradação GPS com resposta de segurança dentro e fora do prazo, paragem cumprida e ignorada, movimento atrasado depois de `fallback`, falhas simultâneas, perda intermitente com recuperação, missão segura incompleta e missão insegura concluída. A combinação de resultados mostra por que motivo conclusão da missão, progresso e segurança são dimensões separadas.

## Desempenho comparável

Ensaio local em Windows, mesma máquina, missão básica, semente 42, 10 000 iterações e 240 000 passos simulados. O binário M2 foi conservado antes do desenvolvimento do M3. O M3 foi executado com contratos nominais e sem falhas de atuador. Três amostras por versão, sem alegar melhoria com base em poucas medições:

| Medida | M2 | M3 |
| --- | --- | --- |
| Tempos totais, em segundos | 5,254133; 5,309840; 5,863256 | 5,229538; 5,187796; 5,146787 |
| Mediana total | 5,309840 s | 5,187796 s |
| Mediana do motor de simulação | incluída no tempo total | 4,126171 s |
| Mediana de preparação e hash dos artefactos | incluída no tempo total | 0,923356 s |
| Mediana do monitor temporal | não aplicável | 0,138269 s, cerca de 2,7% do total |
| Passos por segundo, com base na mediana total | cerca de 45 200 | cerca de 46 264 |
| Pico de memória de trabalho observado | 6 451 200 bytes | 6 897 664 bytes |

A mediana total M3 foi 2,3% inferior nesta pequena amostra; não se atribui essa diferença à implementação. O pico observado aumentou cerca de 446 kB, ou 6,9%. A memória foi amostrada pelo PowerShell a cada 25 ms. O custo de monitorização medido foi 13,8 μs por iteração. A campanha M3 demorou 3,1714 s e a redução deste contraexemplo demorou 0,1331 s. O tempo de reprodução é contabilizado separadamente pela campanha; o workflow publica os artefactos por plataforma para comparar resultados Linux e Windows. A medição do motor e do monitor não inclui compilação, leitura de configuração, escrita final dos ficheiros nem coordenação Python.

## Verificação e limites

Em Windows passaram `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, 83 testes Rust de destino e o teste de documentação. Os 69 testes Python passaram. `cargo llvm-cov --all-targets --fail-under-lines 80` mediu 87,63% de cobertura de linhas. Os workflows Linux e Windows executam os mesmos testes, campanhas, minimizadores e verificações de compatibilidade; a comparação remota de M3 deve ser consultada no workflow da PR antes de considerar a validação cruzada concluída.

O exemplo verifica um modelo determinístico finito e as entradas configuradas. Não demonstra segurança para todo o futuro possível, não modela motores ou dinâmica física e não constitui certificação de segurança no mundo real. Uma campanha aprovada confirma que resultados e expectativas deste conjunto de casos são reproduzíveis, não que qualquer veículo ou missão esteja certificado.

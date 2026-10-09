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

O facto observado é que o estado real se deslocou 250 mm para Este no passo 9, apesar do comando de paragem. O contrato `stop_response`, versão 1, registou `FAIL`, `first_trigger_tick: 7`, `deadline_tick: 9`, `first_violation_tick: 9`, condição esperada `vehicle_stationary` e observação `known_false`. A ordem de movimento de origem é o evento 45, a ordem de paragem é o evento 54 e o primeiro registo de violação é o evento 70; os eventos de origem do prazo são 67 a 74. A falha configurada é `stuck_after_stop`, com comportamento observado `continued_movement`. A posição real passou de (3 000, 1 000) mm para (3 250, 1 000) mm. Estes são factos registados. A interpretação causal de que a falha do atuador manteve o movimento apoia-se na sequência de eventos, mas o relatório não apresenta essa interpretação como medição independente. O hash do registo original é `d9bcdb476f651bff9650762b7cb3375a39d9c060953f9999a61093257cf8cc7e`; o artefacto reduzido conserva a mesma falha e tem o hash `36e276570cc256120abb9fa435eec20357d243581180548198c82aa53e7f2b97`.

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

O minimizador reduziu o horizonte de 16 para 9 passos. Conservou `stop_response`, o operador `bounded_response`, o gatilho no passo 7, o prazo no passo 9, a primeira violação no passo 9 e o mecanismo `continuing_actuation`. O monitor escolhe o gatilho da obrigação falhada mesmo quando uma obrigação anterior do mesmo contrato passou. Também representa falhas temporais sem falha do atuador como `no_actuator_fault_observed`. Os eventos original e reduzido foram reproduzidos em Linux e Windows; ambos mantêm a mesma identidade da falha e os mesmos hashes. A pesquisa verificou um candidato e terminou em 0,0493 s em Linux e 0,0661 s em Windows. É uma redução limitada, não uma garantia de mínimo global.

| Hash SHA-256 | Original | Reduzido |
| --- | --- | --- |
| Identidade canónica `source_artifact_sha256` em `verification.json` | `d9bcdb476f651bff9650762b7cb3375a39d9c060953f9999a61093257cf8cc7e` | `36e276570cc256120abb9fa435eec20357d243581180548198c82aa53e7f2b97` |
| Bytes de `events.json` | `29977dcfbab0c1ade9100a85ba7904ffc36113a96aba5a4b5372790e21f78d2f` | `b6e19190355a22e50a6590557204ab387d3c0da1111b0d92f790ed253be025da` |
| Bytes de `verification.json` | `48dd68a154d192d94f78c8ff4d7fe8f060d3a82ee929827b991e053bb28d6b70` | `409069f4393657396ebfd13253d5398332dd5d167a89fa17add6e791f5dc80b2` |

O hash canónico é o campo `artifact_sha256` do artefacto de eventos v1 e liga o sidecar ao registo reproduzido. Os hashes de bytes permitem validar os ficheiros concretos apresentados acima. A configuração original tem hash `cbdfdbce7539c419735afa66d2e83d3e44629f60e372fc2350f940d722baa484`; o conjunto de contratos tem hash `121735ca65a08894ae912f72796f3ae3a7195b21ac297910e42da25d6d69720e`; a configuração do atuador tem hash `aa0d31c707bf8feaac3daf1088ec8f9c116188d3b1d4ed6379e109cf8adff165`. Os hashes dos artefactos, incluindo `verification.json`, e os comandos completos ficam em `runs/m3-stop-counterexample/summary.json`.

## Campanha M3

Execução de CI em 09/10/2026, [workflow 37934787966](https://github.com/WhiteBlindness/autonomous-systems-vv-lab/actions/runs/37934787966), em Ubuntu 24.04 e Windows, com as sementes 42, 1337 e 2026, dez cenários, limite de 30 casos, 90 invocações, 64 MiB de artefactos e 120 s:

```powershell
python scripts/run_campaign.py --binary target/release/vv-lab.exe --suite m3 --seeds 42,1337,2026 --max-runs 90 --max-cases 30 --max-artifact-bytes 67108864 --time-budget-seconds 120 --output-root runs/m3-campaign
```

Os 30 casos previstos passaram as expectativas em ambos os sistemas. Cada campanha fez 30 repetições determinísticas e 30 reproduções, sem resultados inesperados. A execução de ponta a ponta demorou 1,0847 s em Linux e 1,4823 s em Windows. O tempo somado dos processos de reprodução foi 0,3168 s em Linux e 0,3873 s em Windows. Os resultados agregados foram:

| Dimensão | Resultado |
| --- | --- |
| Contratos temporais | 9 `PASS`, 18 `FAIL` esperados e 3 `INCONCLUSIVE` esperados |
| Segurança | 21 `PASS` e 9 casos com violações esperadas |
| Progresso da missão | 15 concluídas e 15 incompletas |
| Falhas temporais por contrato | `command_applied`: 3; `command_resolution`: 3; `fallback_response`: 3; `mission_progress`: 3; `no_motion_in_fallback`: 6; `restricted_zone_clear`: 3; `stop_response`: 3 |

A campanha de regressão M1/M2 passou 58 casos e 216 invocações por plataforma. As 58 repetições e 58 reproduções passaram, sem resultados inesperados. As verificações dos hashes de referência M1 e M2 passaram, assim como a comparação dos resultados canónicos entre Linux e Windows. O SHA-256 do conjunto de impressões digitais dessa campanha foi `430FA8FBB0A09CE8BF9B2DDD9F3E191270E96F5745BBCDF73DDA3341ECAB8C7C`; o conjunto M3 foi `681C3969BCDEF961680CAEC8AE621B0180212214A5D74BAB09B4D380B8EFACD6`.

O catálogo inclui controlo nominal, degradação GPS com resposta de segurança dentro e fora do prazo, paragem cumprida e ignorada, movimento atrasado depois de `fallback`, falhas simultâneas, perda intermitente com recuperação, missão segura incompleta e missão insegura concluída. A combinação de resultados mostra por que motivo conclusão da missão, progresso e segurança são dimensões separadas.

## Desempenho comparável

Ensaio local em Windows 11 Pro, AMD Ryzen 7 7800X3D, Rust 1.99.0 e destino `x86_64-pc-windows-msvc`. Usou a missão básica, semente 42, 10 000 iterações e 240 000 passos simulados, com três amostras por versão. O binário M2 foi reconstruído a partir do commit `e5317b289367d51d877ad557e27666b12b54fbc8`; o binário M3 foi reconstruído a partir do commit `3308c9759c08d6be36e85da0a6353fd55539df55`. O M3 usou contratos nominais e configuração de atuador sem falhas. Ambos produziram o hash final `6d4b9fd7f65bd1c253569aaf7002f8d9a2a46051f83b462ddbd3a22c6b459136`.

| Medida | M2 | M3 |
| --- | --- | --- |
| Tempo interno por amostra, em segundos | 5,277954; 5,144088; 5,169455 | 5,325645; 5,269129; 5,217035¹ |
| Mediana do tempo total | 5,169455 s | 5,269129 s |
| Mediana da simulação | incluída no tempo total | 4,195073 s |
| Mediana de construção e hash do artefacto | incluída no tempo total | 0,919367 s |
| Mediana do monitor temporal | não aplicável | 0,142880 s, 2,71% do total |
| Passos por segundo, com base na mediana total | cerca de 46 427 | cerca de 45 548 |
| Mediana do pico de memória de trabalho observado | 6 287 360 bytes | 6 914 048 bytes |

A mediana do tempo interno M3 foi 1,9% superior à M2 nesta amostra curta; três medições não permitem concluir que exista uma regressão sustentada. O pico mediano de memória aumentou 626 688 bytes, cerca de 10%. O PowerShell amostrou o conjunto de trabalho a cada 25 ms. O monitor temporal usou cerca de 14,3 μs por iteração. A medição M3 devolve simulação, construção e hash do artefacto e avaliação temporal em campos separados. O tempo de construção inclui o digest do artefacto; os hashes gerados por evento ficam incluídos na simulação. As medições internas não incluem compilação, arranque do processo, escrita de ficheiros nem coordenação Python. A campanha e as reproduções são medidas à parte acima.

Comandos usados em Windows PowerShell. Executar os dois comandos de benchmark três vezes cada:

```powershell
New-Item -ItemType Directory -Path .local -Force
git archive --format=tar --output .local\m2-e5317b2-bench.tar e5317b289367d51d877ad557e27666b12b54fbc8
New-Item -ItemType Directory -Path .local\m2-e5317b2-bench -Force
tar -xf .local\m2-e5317b2-bench.tar -C .local\m2-e5317b2-bench
Push-Location .local\m2-e5317b2-bench
cargo build --release
Pop-Location
cargo build --release
.local\m2-e5317b2-bench\target\release\vv-lab.exe benchmark scenarios/basic-mission.json --seed 42 --iterations 10000
target\release\vv-lab.exe benchmark scenarios/basic-mission.json --seed 42 --iterations 10000 --contracts scenarios/m3/contracts-nominal.json --actuator scenarios/m3/actuator-none.json
```

¹ O total por amostra M3 é a soma das três durações internas medidas nessa execução: simulação, construção do artefacto e monitor.

## Verificação e limites

No [workflow 37934787966](https://github.com/WhiteBlindness/autonomous-systems-vv-lab/actions/runs/37934787966), Linux e Windows passaram formatação, Clippy sem avisos, 84 testes Rust e um teste de documentação. Os 72 testes Python passaram nos dois sistemas. Em Linux, `cargo llvm-cov --locked --all-targets --fail-under-lines 80` mediu 87,95% de cobertura de linhas, acima do limiar de 80%; a auditoria de dependências verificou 22 pacotes bloqueados e passou. A compilação de lançamento, as campanhas M1/M2 e M3, as reproduções original e reduzida, ambos os minimizadores e a comparação canónica Linux/Windows também passaram.

O exemplo verifica um modelo determinístico finito e as entradas configuradas. Não demonstra segurança para todo o futuro possível, não modela motores ou dinâmica física e não constitui certificação de segurança no mundo real. Uma campanha aprovada confirma que resultados e expectativas deste conjunto de casos são reproduzíveis, não que qualquer veículo ou missão esteja certificado.

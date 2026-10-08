# Resultados do M1

Medições e verificações executadas em 08/10/2026. O laboratório usa um pacote Rust, interface de linha de comandos e campanha Python sem dependências adicionais.

## Cenários demonstrados

Resultados para a semente 42:

| Cenário | Resultado dos invariantes | Evidência |
| --- | --- | --- |
| `basic-mission` | PASS | 24 passos, 194 eventos; missão concluída no passo 13, posição final `(4000, 4000)` mm |
| `gps-dropout` | PASS | Sem observações GPS nos passos 4 a 7; ruído limitado a 75 mm por eixo; entrada em `fallback` no passo 6 e recuperação no 10 |
| `communication-fault` | PASS | Perda de pacotes nos passos 3 a 5; atrasos entre 1 e 3 passos; amostra 2 aceite no passo 3, amostra 1 rejeitada no 4 por estar desatualizada |
| `restricted-zone-failure` | FAIL esperado | `restricted_zone` falha primeiro no passo 4: segmento `(2500, 4250)` para `(3000, 4250)` toca na fronteira |
| `restricted-zone-boundary` | FAIL esperado | Contacto com a fronteira é uma violação, mesmo sem atravessar o interior |
| `safe-fallback-failure` | FAIL esperado | `safe_fallback` falha primeiro no passo 3, com a transição para `fallback` deliberadamente desativada; a ordem de imobilidade mantém-se |
| `invalid-transition` | FAIL esperado | `valid_state_transitions` rejeita `pending` para `completed` no passo 1 |
| `world-bounds-failure` | FAIL esperado | Ruído permite uma decisão aparentemente dentro dos limites; o verificador deteta a posição real fora deles |

PASS significa que os invariantes passaram durante o horizonte configurado. Os cenários de GPS e comunicação não concluem a missão nesse horizonte. O controlador segue observações e não planeia uma rota para evitar o polígono restrito.

## Campanha

```sh
cargo build --locked --release
python scripts/run_campaign.py --seeds 42,1337,2026 --output-root runs/campaign
```

Foram executados 8 cenários com 3 sementes: 24 casos, cada um com execução, repetição e reprodução. Todos corresponderam aos resultados esperados. As execuções tiveram 9 PASS de invariantes e 15 FAIL deliberados. Não houve falhas inesperadas, divergências entre os bytes canónicos, falhas de reprodução ou erros de infraestrutura.

| Invariante | Casos PASS | Casos FAIL esperados | Casos FAIL inesperados |
| --- | ---: | ---: | ---: |
| `restricted_zone` | 18 | 6 | 0 |
| `safe_fallback` | 21 | 3 | 0 |
| `valid_state_transitions` | 21 | 3 | 0 |
| `world_bounds` | 21 | 3 | 0 |

O resumo da campanha registou 1,5824 segundos de tempo real total. É uma medição desta execução, incluindo processos e ficheiros, sem significado de desempenho garantido. Cada diretório da campanha contém `summary.json`, `fingerprints.json` e a evidência por cenário e semente.

## Determinismo e reprodução

Os 24 pares de execuções produziram `events.json` e `report.json` idênticos byte a byte. As 24 reproduções recalcularam a simulação e os invariantes com sucesso, incluindo as execuções com violações.

Os testes rejeitam eventos alterados, removidos e reordenados, alterações de configuração e de relatório e uma adulteração de ruído com hashes recalculados. Também rejeitam versões, campos e limites inválidos.

Referência SHA-256 dos bytes de `events.json` para `basic-mission`, semente 42:

```text
42fe5c962ec0475cc504fe0e7a1645b47dab9346985e9e3ff2704aa9f7d3ebbf
```

Referência SHA-256 dos bytes de `report.json`:

```text
26974ce151ea817bbcdc2aa42d7cc37d6acb0dd10b340ea9f84a778fda60bb3a
```

[Referências para os oito cenários](../scenarios/canonical-seed-42.json). A [integração contínua](../.github/workflows/ci.yml) executa a campanha nas sementes 42 e 1337 em Linux e Windows, verifica estas referências e compara os hashes das duas plataformas. Esta comparação cobre os cenários e plataformas testados; não constitui uma garantia para qualquer plataforma ou versão futura.

## Testes e cobertura

- 12 testes unitários Rust e 15 testes de integração através da CLI: 27 aprovados.
- 6 testes Python: aprovados.
- `cargo fmt --check`, `cargo clippy --locked --all-targets --all-features -- -D warnings` e compilação `release`: aprovados.
- `cargo audit`: 21 dependências verificadas, sem avisos publicados na consulta executada.
- Cobertura Rust de linhas: 89,44%; regiões: 92,01%, com `cargo-llvm-cov` 0.9.1. A medição inclui biblioteca e CLI; não mede cobertura de ramos. O CI Linux exige pelo menos 80% de linhas.

```sh
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --locked --version 0.9.1
cargo llvm-cov --locked --all-targets --summary-only --fail-under-lines 80
python -m unittest discover -s tests -p "test_*.py"
```

## Desempenho medido

Máquina: AMD Ryzen 7 7800X3D, 8 núcleos e 16 processadores lógicos; 31,7 GiB de memória reportados pelo sistema; Windows 11 Pro 64 bits, versão 10.0.26300. Compilador: Rust 1.99.0, alvo `x86_64-pc-windows-msvc`, LLVM 23.1.1. Perfil Cargo `release`, sem opções adicionais de otimização.

Comando do executável medido neste sistema Windows, após `cargo build --locked --release`:

```powershell
./target/release/vv-lab.exe benchmark scenarios/basic-mission.json --seed 42 --iterations 10000
```

Três execuções do comando, com aquecimento de uma missão antes de cada medição. Cada execução mediu 10 000 missões de 24 passos: 240 000 passos.

| Execução | Tempo medido, em segundos | Passos por segundo |
| --- | ---: | ---: |
| 1 | 5,260275 | 45 625 |
| 2 | 5,275964 | 45 489 |
| 3 | 5,176048 | 46 367 |

Mediana: cerca de 45 625 passos por segundo. O intervalo medido foi de 45 489 a 46 367. A medição inclui evolução, verificação, construção de eventos, hashes e alocações; exclui compilação, leitura do cenário, aquecimento e escrita dos artefactos. A missão simples termina no passo 13, mas o motor continua a avaliar os invariantes até ao passo 24. Não houve isolamento de processos do sistema nem controlo de frequência do processador. Estes valores não representam outros cenários, física real ou outras máquinas.

As três execuções devolveram o mesmo hash final da cadeia de eventos:

```text
6d4b9fd7f65bd1c253569aaf7002f8d9a2a46051f83b462ddbd3a22c6b459136
```

## Limites e próximo marco

O modelo usa movimento cardinal discreto, chegada por distância de Manhattan e confiança sintética. Não garante convergência para todos os pontos de passagem. O estado interno agrega verdade e observações, embora o controlador não leia a verdade; uma interface de tipos mais restrita pode reforçar essa fronteira.

Os hashes garantem integridade comparável, sem autenticar a origem. A reprodução exige a mesma versão do gerador e das regras de execução. Não há validação contra um veículo real nem evidência de certificação ou segurança operacional.

O próximo marco deve ampliar o catálogo de falhas e as campanhas limitadas, com combinações de ruído, atrasos e janelas, minimização de cenários que falham e critérios explícitos de progresso da missão. Preservar a reprodução exata antes de introduzir interfaces externas.

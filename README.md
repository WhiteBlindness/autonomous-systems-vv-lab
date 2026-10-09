# Autonomous Systems V&V Lab

Laboratório em Rust para verificar missões de um veículo autónomo terrestre em duas dimensões. Combina falhas de sensores, comunicação e atuadores com invariantes de segurança, contratos temporais e reprodução determinística. Python coordena campanhas limitadas e a redução de contraexemplos.

**Estado: M3, protótipo de verificação e campanhas limitadas.** Rust executa a simulação e avalia os contratos; Python seleciona cenários, organiza campanhas e reduz contraexemplos. O controlador recebe observações, nunca o estado real. O laboratório não controla equipamento real.

## Executar uma missão

Requisitos: Rust e Cargo na versão indicada em `rust-toolchain.toml`. Python 3.10 ou superior para campanhas e minimização; não exige bibliotecas adicionais.

```sh
cargo run -- run scenarios/basic-mission.json --seed 42 --output runs/basic
cargo run -- replay runs/basic/events.json
cargo test
```

Cada execução grava `events.json`, `report.json` e `mission.json`. Com `--contracts`, grava também o artefacto versionado `verification.json`. A saída de `run` mantém o contrato do M1: `0` quando os invariantes passam, `2` quando há violações e `1` para erros de configuração, integridade ou execução. Uma falha temporal fica no artefacto de verificação e não altera os códigos de saída legados. Uma missão pode ter segurança PASS e ficar incompleta ou estagnada. A conclusão também não apaga uma violação de segurança.

`mission.json` confirma a chegada física aos pontos de passagem por ordem e apresenta o resultado `completed`, `incomplete`, `stalled` ou `invalid_terminated`. Inclui progresso, tempo em paragem de segurança e motivo da classificação. O controlador usa apenas observações; esta confirmação pertence à análise independente.

## Injetar e reproduzir falhas

```sh
cargo run -- run scenarios/gps-dropout.json --seed 42 --output runs/gps
cargo run -- run scenarios/communication-fault.json --seed 42 --output runs/communication
cargo run -- run scenarios/restricted-zone-failure.json --seed 42 --output runs/failure
cargo run -- replay runs/failure/events.json
```

O terceiro comando termina com código `2`: a trajetória entra na zona restrita. A reprodução termina com código `0` se reconstruir exatamente essa execução, incluindo as violações. Consultar `report.json` para o identificador do invariante, passo, estado real e observado, causa e condição esperada e observada.

`safe-fallback-failure.json` e `invalid-transition.json` ativam alterações deliberadas do controlador, identificadas em `validation_mutants`, para provar que o verificador deteta a falta de entrada no estado de segurança `fallback` e uma transição inválida. São cenários de validação do verificador.

Primeira violação da zona restrita, com campos selecionados do relatório da semente 42:

```json
{
  "name": "restricted_zone",
  "passed": false,
  "failures": [{
    "tick": 4,
    "truth_position": { "x_mm": 3000, "y_mm": 4250 },
    "trigger_event_sequence": 29,
    "trigger_event_kind": "tick_result",
    "expected": "truth position and swept segment outside restricted polygon",
    "observed_result": "from=(2500,4250); to=(3000,4250)"
  }]
}
```

## Arquitetura

```mermaid
flowchart LR
    Config[Cenário e semente] --> Sim[Relógio lógico e escalonador]
    Sim --> Truth[Veículo: estado real]
    Truth --> GPS[GPS: observações e falhas]
    GPS --> Comm[Comunicação: perda e atraso]
    Comm --> Sensor[Estado observado e confiança]
    Sensor --> Mission[Controlador e missão]
    Mission --> Actuator[Atuador e falhas M3]
    Actuator --> Truth
    Truth --> Verify[Verificador independente]
    Sensor --> Verify
    Mission --> Verify
    Sim --> Events[Eventos ordenados e integridade]
    Verify --> Report[Relatório e telemetria]
    Truth --> Progress[Progresso independente da missão]
    Progress --> MissionReport[mission.json]
    Events --> Replay[Reexecução dos eventos]
    Replay --> Verify
    Replay --> Progress
    Report --> Campaign[Campanhas Python limitadas]
    MissionReport --> Campaign
    Campaign --> Reduce[Redução com identidade da falha]
    Reduce --> Config
```

Um pacote Rust mantém as fronteiras entre simulação, veículo, sensores, falhas, missão, verificação, reprodução e interface de linha de comandos. O controlador decide a partir das observações e não lê a posição real. O motor usa-a para mover o veículo e gerar amostras; o verificador usa-a para avaliar os invariantes.

Os quatro invariantes de segurança verificam:

- **`restricted_zone`**: nem a posição inicial nem o segmento percorrido podem tocar ou entrar no polígono restrito.
- **`world_bounds`**: a posição e o segmento percorrido mantêm-se dentro dos limites do mundo.
- **`safe_fallback`**: confiança abaixo do limiar durante o intervalo configurado exige o estado de segurança de paragem.
- **`valid_state_transitions`**: cada mudança de missão e segurança tem de respeitar o respetivo grafo.

Os limites do ambiente são explícitos e também são verificados. [Contrato do modelo e da reprodução](docs/architecture.md).

Baixa confiança impede movimento logo no primeiro passo. O monitor confirma também que uma ordem de paragem corresponde a ausência de deslocamento real. A verificação das transições compara o grafo, a cadeia de eventos e os estados anteriores e atuais.

## Contratos temporais e atuadores

O M3 acrescenta contratos JSON versionados em `scenarios/m3/`. O esquema aceita apenas operadores e predicados tipados; não avalia expressões ou código fornecido no ficheiro. Rust executa um monitor de traço finito e grava os resultados, a evidência e as referências aos eventos em `verification.json`. Os artefactos v1 `events.json` e `report.json` mantêm-se compatíveis.

```sh
cargo run -- run scenarios/m3/nominal-stop.json --seed 42 --output runs/m3-stop --contracts scenarios/m3/contracts-stop.json --actuator scenarios/m3/actuator-continued-stop.json
cargo run -- replay runs/m3-stop/events.json
```

O primeiro comando demonstra uma falha temporal causada pela continuação do movimento após a ordem de paragem. Como os invariantes de segurança passam, o código de saída de `run` continua a ser `0`; o comando apresenta `temporal status: Fail` e guarda o resultado no artefacto novo. A reprodução volta a avaliar o monitor e valida o artefacto `verification.json`. [Exemplo completo com contraexemplo reduzido e hashes](docs/m3-results.md).

Os operadores disponíveis são `always`, `never`, `bounded_response`, `ordered_transition` e `bounded_progress`. A numeração começa no passo 1. Um limite `N` inclui o passo do gatilho e expira em `gatilho + N`; uma resposta no próprio passo satisfaz o contrato. Um gatilho contínuo qualifica uma obrigação por cada sequência contínua que atinja `trigger_for_ticks`; uma observação falsa ou desconhecida reinicia essa sequência. Respostas a comandos de movimento são correlacionadas pelo identificador do comando. Uma ausência de observação registada pelo simulador é um facto conhecido de localização não fiável; quando falta telemetria no traço sem esse facto, a condição dependente da localização fica desconhecida. `command_resolved` exige uma resolução observada no atuador. Se não houver resolução até ao prazo, o contrato falha e o monitor regista `Timeout` como resultado derivado, sem o atribuir ao atuador. `command_applied` exige movimento físico no prazo, mesmo quando o atuador comunica explicitamente uma falha. Uma falha explícita do atuador pode satisfazer `command_resolved`, mas não `command_applied`. Uma violação demonstrada é `FAIL`; uma propriedade cumprida no intervalo observado é `PASS`; uma obrigação ainda aberta no fim do traço é `INCONCLUSIVE`. A conclusão da missão não encerra obrigações de resposta. Estes resultados descrevem o traço finito observado, não garantem todas as execuções futuras.

O modelo do atuador suporta perda de comandos de movimento, atraso determinístico e continuação do último movimento após uma ordem de paragem. A configuração sem falhas preserva o comportamento do M2. Os eventos do atuador registam o passo do comando de origem, o prazo e o passo de execução. O modelo pode aplicar um comando atrasado depois de o controlador entrar em `fallback`.

Para reduzir e reproduzir o contraexemplo de paragem:

```sh
cargo build --release
python scripts/m3_minimize_failure.py scenarios/m3/nominal-stop.json --contracts scenarios/m3/contracts-stop.json --actuator scenarios/m3/actuator-continued-stop.json --contract-id stop_response --seed 42 --output runs/m3-stop-counterexample
cargo run -- replay runs/m3-stop-counterexample/original/events.json
cargo run -- replay runs/m3-stop-counterexample/reduced/events.json
```

O minimizador mantém o identificador do contrato, o tipo de propriedade, o mecanismo de continuação do movimento, o gatilho, o prazo, a violação e a reprodução determinística. Guarda os artefactos originais e reduzidos.

## Determinismo e integridade

Há dois modos distintos:

1. **Nova execução pela semente:** cenário e semente iguais produzem JSON canónico e somas de controlo iguais.
2. **Reprodução dos eventos:** carrega o cenário e a sequência de entradas, reconstrói a simulação e compara os estados, as transições, a telemetria e os invariantes com o manifesto original.

A reprodução confirma também que as entradas registadas correspondem à semente. Por isso, depende da mesma versão do gerador pseudoaleatório e das regras do modelo.

Coordenadas inteiras, passos fixos, algoritmo pseudoaleatório explícito e ordem total dos eventos evitam dependências de tempo real e diferenças de arredondamento. Os hashes SHA-256 detetam alterações no cenário e no registo. Não são assinaturas: quem substituir deliberadamente todo o artefacto e recalcular os hashes pode criar um novo registo válido. A confiança na origem exige guardar a soma de controlo numa fonte independente.

O formato tem versão e rejeita campos desconhecidos e valores fora dos limites. A compatibilidade entre versões do simulador exige testes; a versão do formato não garante por si só o mesmo comportamento futuro.

## Campanha e verificações

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
python -m unittest discover -s tests -p "test_*.py"
python scripts/run_campaign.py --seeds 42,1337,2026 --output-root runs/campaign
```

`--suite all` executa a matriz de regressão M1/M2. A campanha M3 é uma matriz separada, com limites próprios, para não ultrapassar o orçamento pedido ao executar cada conjunto:

```sh
python scripts/run_campaign.py --suite all --variants standard,short-stall,fault-stress --seeds 42,1337 --max-runs 256 --max-cases 80 --max-artifact-bytes 67108864 --time-budget-seconds 120 --output-root runs/campaign
```

Para executar a matriz M3 predefinida, com dez cenários e três sementes:

```sh
python scripts/run_campaign.py --suite m3 --seeds 42,1337,2026 --max-runs 90 --max-cases 30 --max-artifact-bytes 67108864 --time-budget-seconds 120 --output-root runs/m3-campaign
```

Cada campanha grava JSON, resumo Markdown e evidência por cenário, variante e semente. Verifica a repetição canónica e reproduz cada caso. A campanha M3 apresenta separadamente contratos temporais, segurança e progresso da missão, e identifica contratos e contraexemplos reduzidos.

`--suite m2 --seeds 42 --variants standard` seleciona dez casos. `--max-cases`, `--max-runs`, `--max-artifact-bytes` e `--time-budget-seconds` limitam a campanha antes e durante a execução. `--max-runs` conta invocações do simulador, incluindo repetições, reprodução e análise. Uma expectativa satisfeita não transforma uma missão insegura numa missão bem-sucedida.

GitHub Actions executa formatação, Clippy, testes Rust e Python, cobertura mínima de 80%, as campanhas M1/M2 e M3, minimização e reprodução em Linux e Windows. Compara os hashes canónicos entre os dois sistemas, incluindo os artefactos M3 reduzidos. [Resultados do M3](docs/m3-results.md), [resultados do M2](docs/m2-results.md), [resultados do M1](docs/m1-results.md) e [contrato de reprodução](docs/reproducibility.md).

## Reduzir uma falha

O par `f-zone-nominal.json` e `g-zone-fault.json` mantém missão, geometria e limiar de confiança. O primeiro conclui com segurança PASS. Com ruído de GPS, o segundo toca na zona restrita. Não utiliza `validation_mutants`.

```sh
cargo build --release
cargo run -- run scenarios/m2/g-zone-fault.json --seed 42 --output runs/original
python scripts/minimize_failure.py scenarios/m2/g-zone-fault.json --seed 42 --output runs/minimized --max-candidates 80 --time-budget-seconds 30
cargo run -- run runs/minimized/scenarios/minimized.json --seed 42 --output runs/reduced
cargo run -- replay runs/original/events.json
cargo run -- replay runs/reduced/events.json
```

Os dois comandos `run` terminam com código 2, por violação esperada. Executar os comandos seguintes mesmo após esse código. O script localiza o executável em `target/release`; `--binary` ou `VV_LAB_BINARY` permitem indicar outro caminho.

Na semente 42, a redução verificada diminui o horizonte de 32 para 19 passos e o ruído máximo de 500 para 3 mm. Conserva `restricted_zone`, a transição real do exterior para a fronteira e uma observação com ruído que influencia o movimento. O cenário reduzido reproduz exatamente a sua própria execução; não tem de gerar a mesma trajetória ou o mesmo hash do original.

`runs/minimized/summary.json` e `summary.md` apresentam assinaturas antes e depois, reduções aceites, candidatos rejeitados, limites e comandos concretos. As pastas `original/` e `final/` contêm eventos e relatórios já reproduzidos. Por omissão, a pesquisa conserva a semente e limita-se a 80 candidatos, 30 segundos, 16 MiB retidos e 32 MiB temporários. Se nenhum candidato válido conservar a falha, apresenta esse resultado.

Também é possível selecionar `--property mission:stalled` para reduzir uma estagnação. A pesquisa mantém a classificação, os critérios de progresso e a política. É uma redução prática limitada, sem garantia de mínimo global. [Exemplo M2 com evidência e métricas](docs/m2-results.md) e [catálogo de cenários](docs/scenario-catalogue.md).

## Simplificações deliberadas

- Movimento cinemático numa grelha 2D, com orientação cardinal e velocidade discreta. Sem aceleração, dinâmica dos pneus, colisões entre veículos ou física de terreno.
- Pontos de passagem e controlador simples. Não existe planeamento de trajetórias para evitar a zona restrita; o verificador deteta uma trajetória insegura.
- Confiança do GPS e envelhecimento das observações seguem regras explícitas do modelo. Não representam um estimador probabilístico calibrado.
- As mensagens transportam observações do GPS. Não há protocolo de rede real, múltiplos agentes ou controlo de equipamento.
- A integridade do registo não prova a segurança de um sistema real. O laboratório verifica propriedades deste modelo e das entradas fornecidas.
- Sem interface gráfica, serviços distribuídos ou alojamento. A pesquisa tem uma ordem determinística, mas um orçamento de tempo atingido pode interrompê-la mais cedo numa máquina lenta.
- As regras de identidade da falha são conservadoras e específicas deste modelo. Um M4 poderá avaliar uma interface versionada entre simulador e controlador, mantendo a execução determinística num único processo.

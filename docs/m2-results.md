# Resultados do M2

Medições locais de 08/10/2026 sobre o M1 integrado em `cfadcdb90286d3da512db2b0fa405d60082fae5d`. O M2 acrescenta análise independente do progresso, campanhas de falhas combinadas e redução limitada de cenários. Mantém o modelo físico, os eventos e os relatórios v1.

## Campanha verificada

```sh
cargo build --release
python scripts/run_campaign.py --binary target/release/vv-lab --suite all --seeds 42,1337,2026 --max-cases 100 --max-runs 400 --max-artifact-bytes 67108864 --time-budget-seconds 120 --output-root runs/m2-verified
```

Em Windows, acrescentar `.exe` ao caminho do executável. A campanha cria uma pasta própria e grava `summary.json`, `campaign-report.md`, os hashes e a evidência por caso.

- 87 casos: 24 regressões do M1 e 63 combinações do M2.
- 324 invocações do simulador: 174 execuções, 87 reproduções e 63 análises reconstruídas.
- 87 comparações por repetição, 87 reproduções e 63 comparações de `mission.json` passaram.
- 54 casos com segurança PASS e 33 com violações esperadas. Nenhuma violação inesperada.
- Missões: 27 concluídas, 34 incompletas, 17 estagnadas e nove com terminação inválida. As 63 expectativas explícitas do M2 coincidiram com os resultados; as regressões do M1 também apresentam métricas de missão.
- Violações por invariante: 12 de `restricted_zone`, nove de `world_bounds`, três de `safe_fallback` e nove de `valid_state_transitions`.
- Duração: 9,7187 s. Artefactos retidos: 24 252 583 bytes, abaixo do orçamento de 64 MiB.

Uma expectativa satisfeita significa que o laboratório reproduziu o comportamento previsto. Os 33 casos com violações continuam a ser execuções inseguras no modelo.

## Falhas combinadas e progresso

Resultados da semente 42, variante `standard`:

| Caso | Segurança | Resultado da missão | Evidência |
| --- | --- | --- | --- |
| A: GPS perdido e comunicação atrasada | PASS | Concluída no passo 58 | Amostra do passo 3 entregue no passo 6, dentro da perda de GPS; idade 3, confiança 250; paragem no passo 7 |
| B: recuperação com ruído e perda de pacotes | PASS | Estagnada | Receber amostras recentes com confiança inferior a 500 mantém a paragem; recuperação no passo 23, nova paragem no 25 |
| C: perdas intermitentes | PASS | Concluída no passo 57 | Três ciclos de recuperação; 19 passos em `fallback`; sem deslocamento durante baixa confiança |
| D: perda prolongada | PASS | Estagnada, `expected_fault_hold` | Nenhum destino atingido; 34 passos desde progresso; 33 em `fallback` |
| E: horizonte curto | PASS | Incompleta | Progresso até ao passo 8, mas ainda falta o destino |
| F: referência sem ruído | PASS | Concluída no passo 17 | Mesma missão e geometria do caso G |
| G: ruído junto da zona | FAIL, `restricted_zone` | Incompleta | Primeiro contacto real com a fronteira no passo 27; sem `validation_mutants` |
| H / I: referência e ruído junto do limite | PASS / FAIL, `world_bounds` | Concluída / incompleta | Mesmo limiar e geometria, diferentes observações |
| J: transição deliberadamente inválida | FAIL, `valid_state_transitions` | `invalid_terminated` | Conclusão observada sem chegada física, no passo 1 |

O caso G chegou fisicamente ao destino, mas o controlador não confirmou conclusão. A classificação permanece incompleta; a violação da zona mantém-se. O caso C conserva a primeira deteção de estagnação no histórico, apesar de concluir mais tarde.

Os testes de interação verificam a ordem por prazo e sequência, rejeição de amostras mais antigas e ausência de deslocamento real. Os três ciclos de C não são uma alegação de recuperação garantida para qualquer semente ou configuração.

## Exemplo de redução reproduzida

```sh
python scripts/minimize_failure.py scenarios/m2/g-zone-fault.json --seed 42 --output runs/m2-reduced-final --max-candidates 80 --time-budget-seconds 30
cargo run -- replay runs/m2-reduced-final/original/events.json
cargo run -- replay runs/m2-reduced-final/final/events.json
cargo run -- run runs/m2-reduced-final/scenarios/minimized.json --seed 42 --output runs/reduced
```

Em Windows, foi usado `--binary target/release/vv-lab.exe`. A execução retida terminou em 1,137337 s: 14 candidatos, sete reduções aceites, sete rejeições por alteração da propriedade e nenhuma configuração inválida. Ambos os artefactos foram reproduzidos e as análises de missão reconstruídas coincidiram com as respetivas execuções.

| Medida | Original | Reduzido |
| --- | --- | --- |
| Semente | 42 | 42 |
| Horizonte | 32 passos | 19 passos |
| Ruído máximo de GPS | 500 mm | 3 mm |
| Pontuação de complexidade | 532 | 22 |
| Primeira violação | Passo 27 | Passo 19 |
| Invariante | `restricted_zone` | `restricted_zone` |
| Condição real | Exterior para fronteira | Exterior para fronteira |

A pontuação desce 95,86%; neste exemplo é apenas horizonte mais ruído máximo, porque não existem janelas, probabilidades de perda ou atrasos. O horizonte desce 40,63% e o ruído 99,4%. São métricas de redução do exemplo, não ganhos de desempenho ou de segurança.

Evidência selecionada da falha reduzida:

```json
{
  "tick": 19,
  "truth_position": {"x_mm": 5000, "y_mm": 1750},
  "observed": {"position": {"x_mm": 5001, "y_mm": 2002}, "sample_tick": 19},
  "trigger_event_kind": "tick_result",
  "expected": "truth position and swept segment outside restricted polygon",
  "observed_result": "from=(5000,2000); to=(5000,1750)"
}
```

Original e reduzido conservam a assinatura: único invariante falhado `restricted_zone`, amostra de GPS com ruído e transição do exterior para a fronteira. O verificador permanece ativo. O minimizador exporta cenário original e reduzido, assinaturas, passos aceites, orçamentos, comandos e eventos. Não conserva uma redução que apenas produza uma falha diferente.

Hashes SHA-256 dos bytes do exemplo reduzido:

```text
scenarios/minimized.json  cadf6498490249c0f7499862f741b3bd8839936895ee37f6cc23800e83068ee1
final/events.json         53349e6d3d67e843b84679490ac09dbc07510f174ac0aa3e31960dbd6b999c62
final/report.json         7f260482376cb5046734830e014f0d67e32e9aecc7736b65a452c3dd46f852b1
final/mission.json        979b9b9291394c1b982d74cfd4a96048bfe70f829e8125fb14ec7cfe147debae
```

## Desempenho comparável

Máquina: AMD Ryzen 7 7800X3D, oito núcleos e 16 processadores lógicos, cerca de 31,7 GiB de memória física. Windows 11 Pro de 64 bits, versão 10.0.26300. Rust 1.99.0, destino `x86_64-pc-windows-msvc`, LLVM 23.1.1. Python 3.14.6 nas campanhas.

```sh
vv-lab benchmark scenarios/basic-mission.json --seed 42 --iterations 10000
```

Três amostras por versão, cada uma com 240 000 passos simulados, compilação de lançamento e o mesmo cenário, semente e hash final. O executável M1 foi compilado e conservado antes das alterações.

| Versão | Tempos das três amostras, em segundos | Mediana | Passos por segundo na mediana |
| --- | --- | --- | --- |
| M1 | 5,324819; 5,114858; 5,062190 | 5,114858 s | 46 922 |
| M2 | 5,098322; 5,133955; 5,144638 | 5,133955 s | 46 748 |

A diferença observada na mediana do tempo é de +0,37%. Estas três amostras não permitem atribuir uma diferença tão pequena a uma regressão. O ensaio inclui geração de eventos, estados, invariantes e hashes em memória. Exclui compilação, leitura do cenário, escrita de ficheiros e análise adicional da missão. Há uma execução de aquecimento antes do intervalo medido; os processos não foram isolados da restante atividade da máquina.

## Verificação técnica

Formatação, Clippy com avisos tratados como erros e os 44 testes Rust passaram: 12 testes unitários, 15 integrações M1, 16 integrações M2 e uma prova de compilação que rejeita acesso à posição real pelo controlador. A cobertura de linhas medida por `cargo-llvm-cov` é 2 061 de 2 365, ou 87,15%, acima do limiar de 80%. `cargo audit` não encontrou vulnerabilidades nem avisos nas 22 dependências contabilizadas do ficheiro de bloqueio.

A revisão técnica independente verificou fronteira de observações, conclusão física, estagnação, interação das falhas, integridade da reprodução e identidade da redução. A redução compara a primeira evidência de falha e conserva as categorias de perturbação; os testes incluem rejeição de uma condição diferente seguida por uma evidência coincidente. Os 39 testes Python passaram também numa cópia limpa do repositório, sem pastas de execuções anteriores.

O workflow [verification](https://github.com/WhiteBlindness/autonomous-systems-vv-lab/actions/workflows/ci.yml) aplica as verificações em Linux e Windows. A campanha de CI usa 58 casos e 216 invocações; a comparação entre plataformas cobre os oito pares M1 fixados, os 21 casos M2 da semente 42 e os quatro hashes do cenário reduzido: cenário, eventos, invariantes e missão. O resultado remoto pertence à PR e identifica o commit verificado.

## Compatibilidade e limites do modelo

Os oito pares de hashes canónicos do M1 passaram sem alterar `scenarios/canonical-seed-42.json`. Para a missão básica da semente 42:

```text
events.json  42fe5c962ec0475cc504fe0e7a1645b47dab9346985e9e3ff2704aa9f7d3ebbf
report.json  26974ce151ea817bbcdc2aa42d7cc37d6acb0dd10b340ea9f84a778fda60bb3a
```

O limiar permissivo dos pares de violações naturais expõe as limitações do controlador; não representa uma configuração recomendada para um veículo. Os casos de recuperação usam limiares significativos e verificam observações pouco confiantes. A análise confirma propriedades deste modelo; não constitui certificação de segurança, validação de equipamento ou prova de autonomia operacional.

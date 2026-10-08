# Regras do repositório

## Âmbito

Este laboratório verifica missões de um veículo autónomo terrestre genérico, sem armamento. Rust implementa a simulação, os sensores, as falhas, os invariantes e a reprodução de eventos. Python serve apenas para campanhas limitadas e análise.

Manter um pacote Rust pequeno. Interfaces distribuídas, visualização, controlo de equipamento e modelos físicos complexos ficam fora do M1.

## Contrato de determinismo

- Usar relógio lógico, passos fixos, aritmética inteira e geração pseudoaleatória com algoritmo e semente explícitos.
- Não usar tempo real, rede, aleatoriedade sem semente nem ordem de execução de tarefas na evolução da simulação.
- Separar estado real de observações. O controlador só recebe observações e configuração da missão.
- Definir uma ordem total dos eventos. Validar configuração, versões, limites e integridade antes da reprodução.
- A reprodução deve reconstruir estados e resultados dos invariantes. Não devolver conclusões guardadas sem as recalcular.
- Cada falha deve identificar invariante, instante lógico, estado, causa e condição esperada e observada.
- Medir desempenho fora da simulação. Publicar apenas medições executadas, com máquina, comando e limites.

## Verificação

Antes de concluir alterações, executar:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
python scripts/campaign.py --help
```

Executar também os cenários e a campanha documentados no README. Uma violação deliberada é um resultado esperado do cenário, não uma licença para ignorar falhas dos testes. Acrescentar testes de regressão para alterações de geometria, confiança, ordem dos eventos e reprodução.

## Apresentação pública

Descrever o produto atual, decisões técnicas, resultados verificáveis e limitações materiais. Usar mensagens de commit concisas e orientadas ao resultado, no formato `tipo: descrição`. Não publicar história de redação, transcrições, instruções privadas, caminhos pessoais, segredos, credenciais ou alegações sem evidência.

Rever o diff preparado, documentação, testes e mensagem antes de cada commit e envio. Não reescrever história apenas para alterar linguagem antiga. Documentar problemas reais de segurança e correção com precisão.

Publicar, enviar alterações ou integrar uma PR apenas quando a tarefa o autorizar. Não integrar automaticamente PRs. Preservar alterações de outros colaboradores; atribuir um único responsável à simulação quando houver trabalho paralelo.

## Documentação

Usar português europeu, Acordo Ortográfico de 1990, frases claras e títulos em formato de frase. Preservar identificadores técnicos e comandos. Não usar o carácter U+2014. `AGENTS.md` é a fonte comum de regras; `CLAUDE.md` importa este ficheiro.

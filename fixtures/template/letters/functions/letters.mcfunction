## Synthetic: `if` with methods, f-strings
foreach <i block blocks>:
    if <block.startswith("demo:letter_sv_")>:
        execute if block ~ ~ ~ `eval:block` run tag @s add sv_`eval:i`
    if <block.endswith('_a') or block.startswith("demo:letter_en_")>:
        execute if block ~ ~ ~ `eval:block` run tag @s add en_`eval:i`
foreach <_ name names>:
    execute if entity @s[family=letter_`eval:name`] run dialogue open @e[type=demo:bear,c=1] @a[c=1] `eval:f'bear_correct_{name}'`
    clear @s `eval:f"demo:{name.lower()}_crop_item"`
    say `eval:f'{name.upper()} has {len(name)} letters, {{braces}}'`
    scoreboard players set @s len `eval:int(str(len(name)) + '0')`
    say `eval:name.lower() + str(1.5)`
function <check>:
    if <count >= 2>:
        say many
    if <count < 2>:
        say few

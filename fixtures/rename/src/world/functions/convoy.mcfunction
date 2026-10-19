# a convoy: one lap function per car (a template, see scope.json)
for <i 0..3>:
    execute as @e[type=demo:car] run function <lap_`eval:i`>:
        say `eval:label` `eval:i`
        summon demo:car ~ ~ ~
definefunction <honk>:
    execute as @e[type=demo:car] run say beep

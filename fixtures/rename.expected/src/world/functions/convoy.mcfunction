# a convoy: one lap function per car (a template, see scope.json)
for <i 0..3>:
    execute as @e[type=demo:truck] run function <lap_`eval:i`>:
        say `eval:label` `eval:i`
        summon demo:truck ~ ~ ~
definefunction <honk>:
    execute as @e[type=demo:truck] run say beep

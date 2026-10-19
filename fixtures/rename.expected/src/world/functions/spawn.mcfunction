# cross-module references to the vehicles module
summon demo:truck ~ ~1 ~
execute as @e[type=demo:truck] run say vroom
give @s demo:horn

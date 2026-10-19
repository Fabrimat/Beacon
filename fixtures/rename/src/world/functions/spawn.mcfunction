# cross-module references to the vehicles module
summon demo:car ~ ~1 ~
execute as @e[type=demo:car] run say vroom
give @s demo:horn

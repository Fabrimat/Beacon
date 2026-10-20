execute if block ~ ~ ~ demo:letter_sv_a run tag @s add sv_0
execute if block ~ ~ ~ demo:letter_sv_a run tag @s add en_0
execute if block ~ ~ ~ demo:letter_en_b run tag @s add en_1
execute if block ~ ~ ~ demo:stone_a run tag @s add en_2
execute if entity @s[family=letter_Scarf] run dialogue open @e[type=demo:bear,c=1] @a[c=1] bear_correct_Scarf
clear @s demo:scarf_crop_item
say SCARF has 5 letters, {braces}
scoreboard players set @s len 50
say scarf1.5
execute if entity @s[family=letter_Hat] run dialogue open @e[type=demo:bear,c=1] @a[c=1] bear_correct_Hat
clear @s demo:hat_crop_item
say HAT has 3 letters, {braces}
scoreboard players set @s len 30
say hat1.5
function letters/letters/check
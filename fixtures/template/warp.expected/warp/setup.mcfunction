# Prepare the warp
scoreboard objectives add warp dummy
scoreboard players set point_0 warp 0
scoreboard players set point_1 warp 1
scoreboard players set point_2 warp 2
execute as @a run function warp/warp/setup/reset
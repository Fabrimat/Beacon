# Sets up scoreboards and constants.
scoreboard objectives add demo_tmp dummy
scoreboard objectives add demo_tmp1 dummy
scoreboard objectives add demo_tmp2 dummy
scoreboard objectives add demo_tmp3 dummy
# Constants `eval:not_evaluated`
scoreboard players set _-1 demo_const -1
scoreboard players set _0 demo_const 0
scoreboard players set _1 demo_const 1
scoreboard players set _2 demo_const 2
say countdown 6
say countdown 3
say done
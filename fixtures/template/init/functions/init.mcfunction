# Sets up scoreboards and constants.

## internal note, dropped
scoreboard objectives add demo_tmp dummy
for <i 1..4 1>:
    scoreboard objectives add demo_tmp`eval:i` dummy

# Constants `eval:not_evaluated`
for <i -1..3>:
    scoreboard players set _`eval:i` demo_const `eval:i`
for <k 6..0 -3>:
    say countdown `eval:k`
say done
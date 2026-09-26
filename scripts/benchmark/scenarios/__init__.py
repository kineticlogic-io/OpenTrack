"""Every benchmark scenario, by name: name -> function that builds it."""

from scenarios import ferry, gmti, load, stonesoup, synthetic

SCENARIOS = {}
for _m in (ferry, stonesoup, synthetic, gmti, load):
    SCENARIOS.update(_m.SCENARIOS)

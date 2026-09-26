"""Every benchmark scenario, by name: name -> function that builds it."""

from scenarios import ferry, gmti, stonesoup, synthetic

SCENARIOS = {}
for _m in (ferry, stonesoup, synthetic, gmti):
    SCENARIOS.update(_m.SCENARIOS)

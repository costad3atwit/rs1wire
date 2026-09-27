from w1thermsensor import W1ThermSensor, Sensor
import time

start = time.perf_counter()
for sensor in W1ThermSensor.get_available_sensors():
    print("Sensor %s has temperature %.2f" % (sensor.id, sensor.get_temperature(Unit.DEGREES_F)))

elapsed = time.perf_counter() - start
from w1thermsensor import W1ThermSensor, Sensor, Unit
import time

sensors = W1ThermSensor.get_available_sensors()
sensor_results = [None] * len(sensors)
start = time.perf_counter()
for i, sensor in enumerate(sensors):
    sensor_results[i] = (sensor.id, sensor.get_temperature(Unit.DEGREES_F))

elapsed = time.perf_counter() - start

for sensor_id, temp_f in sensor_results:
    print("Sensor %s has temperature %.2f" % (sensor_id, temp_f))

print(f"Elapsed: {elapsed}")

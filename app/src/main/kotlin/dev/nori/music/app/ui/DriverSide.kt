package dev.nori.music.app.ui

import android.content.Context
import android.content.pm.PackageManager

/**
 * Where a car's driver sits, on a car that runs the app on its own screen (Android Automotive). On its side
 * the controls and the words then go on the driver's side and the cover on the other ([LocalCoverAtEnd]):
 * with the wheel on the left, as in most of Europe, the controls are on the left and the cover on the
 * right; with it on the right, as in the UK, the other way round, which is how a phone on its side lays out.
 *
 * Asked once, from the car's own configuration (the driver's occupant zone and its seat). False anywhere else - a
 * phone, a tablet, a car that does not say. Android Auto, the phone's screen projected in a car, never
 * shows the app's own pages (the car draws its media screens itself), and tells an app nothing of the wheel.
 */
object DriverSide {
    @Volatile private var known: Boolean? = null

    fun onLeft(context: Context): Boolean = known ?: ask(context.applicationContext).also { known = it }

    private fun ask(context: Context): Boolean {
        if (!context.packageManager.hasSystemFeature(PackageManager.FEATURE_AUTOMOTIVE)) return false
        return runCatching {
            val car = android.car.Car.createCar(context) ?: return false
            try {
                val zones = car.getCarManager(android.car.Car.CAR_OCCUPANT_ZONE_SERVICE) as? android.car.CarOccupantZoneManager ?: return false
                // The driver's zone and its seat, as the car's own configuration has it.
                zones.allOccupantZones.firstOrNull { it.occupantType == android.car.CarOccupantZoneManager.OCCUPANT_TYPE_DRIVER }
                    ?.seat == android.car.VehicleAreaSeat.SEAT_ROW_1_LEFT
            } finally {
                car.disconnect()
            }
        }.getOrDefault(false)
    }
}

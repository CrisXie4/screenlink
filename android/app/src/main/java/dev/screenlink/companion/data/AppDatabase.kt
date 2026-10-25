package dev.screenlink.companion.data

import android.content.Context
import androidx.room.Database
import androidx.room.Room
import androidx.room.RoomDatabase

/**
 * B3：Room 数据库（DESIGN.md §7 逐字实现）。
 * 版本策略：v1 无迁移；后续加列必须 bump version 并写 Migration，
 * schemas/ 目录随 git 留痕（ksp room.schemaLocation 已配置）。
 */
@Database(entities = [TransferRecord::class], version = 1, exportSchema = true)
abstract class AppDatabase : RoomDatabase() {
    abstract fun transferDao(): TransferDao

    companion object {
        @Volatile private var inst: AppDatabase? = null

        fun get(context: Context): AppDatabase = inst ?: synchronized(this) {
            inst ?: Room.databaseBuilder(context.applicationContext, AppDatabase::class.java, "screenlink.db")
                .build().also { inst = it }
        }
    }
}

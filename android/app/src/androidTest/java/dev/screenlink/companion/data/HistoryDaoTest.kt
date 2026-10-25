package dev.screenlink.companion.data

import androidx.room.Room
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/**
 * B：Room 传输历史 DAO 仪器化测试（DESIGN.md §4 B 测试条目），跑在真机/模拟器（emulator-5554）。
 * 运行：`cd /d/screenlink/android && gradle :app:connectedDebugAndroidTest`
 *
 * 用内存库，不污染 screenlink.db。
 * Context 取自 androidx.test:monitor 的 InstrumentationRegistry：runner 1.6.2 已在
 * app/build.gradle.kts 显式声明，monitor 是其 POM 直接依赖，不新增依赖。
 *
 * 断言刻意使用 TransferRepository 里的 STATUS_xxx 与 DIR_xxx 常量，与 DAO 里硬编码的
 * SQL 字面量（direction 0/1、status 0/1/2/3/4）互相锁定：任一侧改动而另一侧未同步时，
 * 这里的行为断言会失败。
 */
@RunWith(AndroidJUnit4::class)
class HistoryDaoTest {

    private lateinit var db: AppDatabase
    private lateinit var dao: TransferDao

    @Before
    fun setUp() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        db = Room.inMemoryDatabaseBuilder(context, AppDatabase::class.java)
            .allowMainThreadQueries()
            .build()
        dao = db.transferDao()
    }

    @After
    fun tearDown() {
        db.close()
    }

    private fun record(
        id: Long = 0,
        direction: Int = TransferRepository.DIR_RECV,
        fileName: String = "a.jpg",
        status: Int = TransferRepository.STATUS_RUNNING,
        startedAt: Long = 1_000L,
        transferred: Long = 0L,
    ) = TransferRecord(
        id = id,
        direction = direction,
        fileName = fileName,
        sizeBytes = 100L,
        mimeType = "image/jpeg",
        peerName = "DEV-PC",
        peerHost = "192.168.1.10",
        status = status,
        transferredBytes = transferred,
        md5 = null,
        startedAt = startedAt,
        finishedAt = null,
        contentUri = null,
    )

    @Test
    fun insertAndObserveRecent_ordersByStartedAtDesc_andRespectsLimit() = runBlocking {
        dao.insert(record(fileName = "old.jpg", startedAt = 1_000L))
        dao.insert(record(fileName = "new.jpg", startedAt = 3_000L))
        dao.insert(record(fileName = "mid.jpg", startedAt = 2_000L))

        val top2 = dao.observeRecent(limit = 2).first()
        assertEquals(listOf("new.jpg", "mid.jpg"), top2.map { it.fileName })

        val all = dao.observeRecent().first()
        assertEquals(listOf("new.jpg", "mid.jpg", "old.jpg"), all.map { it.fileName })
    }

    @Test
    fun insert_assignsAutoIncrementIds() = runBlocking {
        val id1 = dao.insert(record(fileName = "1.jpg"))
        val id2 = dao.insert(record(fileName = "2.jpg"))
        assertTrue(id1 > 0)
        assertTrue(id2 > id1)
    }

    @Test
    fun updateProgress_updatesTransferredBytesOnly() = runBlocking {
        val id = dao.insert(record(fileName = "video.mp4", transferred = 0))
        dao.updateProgress(id, 42L)

        val row = dao.observeRecent().first().single()
        assertEquals(42L, row.transferredBytes)
        assertEquals(TransferRepository.STATUS_RUNNING, row.status)
        assertNull(row.finishedAt)
    }

    @Test
    fun finish_setsStatusMd5FinishedAtTransferred() = runBlocking {
        val id = dao.insert(record(fileName = "doc.pdf"))
        dao.finish(id, TransferRepository.STATUS_DONE, "d41d8cd988f00b204e9800998ecf8427e", 9_999L, 100L)

        val row = dao.observeRecent().first().single()
        assertEquals(TransferRepository.STATUS_DONE, row.status)
        assertEquals("d41d8cd988f00b204e9800998ecf8427e", row.md5)
        assertEquals(9_999L, row.finishedAt)
        assertEquals(100L, row.transferredBytes)
    }

    @Test
    fun queued_listsSendQueuedRowsOrderedByStartedAt() = runBlocking {
        dao.insert(record(direction = TransferRepository.DIR_SEND, status = TransferRepository.STATUS_QUEUED, fileName = "q1.jpg", startedAt = 1L))
        dao.insert(record(direction = TransferRepository.DIR_RECV, status = TransferRepository.STATUS_QUEUED, fileName = "recv-queued.jpg", startedAt = 2L))
        dao.insert(record(direction = TransferRepository.DIR_SEND, status = TransferRepository.STATUS_DONE, fileName = "send-done.jpg", startedAt = 3L))
        dao.insert(record(direction = TransferRepository.DIR_SEND, status = TransferRepository.STATUS_QUEUED, fileName = "q2.jpg", startedAt = 4L))

        val queued = dao.queued()
        assertEquals(listOf("q1.jpg", "q2.jpg"), queued.map { it.fileName })
    }

    @Test
    fun findResumable_returnsLatestRecvRunningOrFailedRow_only() = runBlocking {
        dao.insert(record(fileName = "video.mp4", status = TransferRepository.STATUS_RUNNING, startedAt = 1L))
        dao.insert(record(fileName = "video.mp4", status = TransferRepository.STATUS_FAILED, startedAt = 2L))
        dao.insert(record(fileName = "video.mp4", status = TransferRepository.STATUS_DONE, startedAt = 3L))
        dao.insert(record(direction = TransferRepository.DIR_SEND, fileName = "video.mp4", status = TransferRepository.STATUS_RUNNING, startedAt = 4L))

        val resumable = dao.findResumable("video.mp4")
        // RUNNING/FAILED 中 startedAt 最新的是 2L 那行（FAILED）；DONE 与 SEND 方向都不可续传
        assertEquals(TransferRepository.STATUS_FAILED, resumable?.status)
        assertEquals(2L, resumable?.startedAt)

        assertNull(dao.findResumable("missing.bin"))
    }

    @Test
    fun failAllRunning_reapsOnlyRunningRows() = runBlocking {
        val runningId = dao.insert(record(fileName = "running.jpg", status = TransferRepository.STATUS_RUNNING))
        val queuedId = dao.insert(record(fileName = "queued.jpg", status = TransferRepository.STATUS_QUEUED))
        val doneId = dao.insert(record(fileName = "done.jpg", status = TransferRepository.STATUS_DONE))
        val failedId = dao.insert(record(fileName = "failed.jpg", status = TransferRepository.STATUS_FAILED))

        val now = 555_555L
        dao.failAllRunning(now)

        val rows = dao.observeRecent().first().associateBy { it.id }
        assertEquals(TransferRepository.STATUS_FAILED, rows.getValue(runningId).status)
        assertEquals(now, rows.getValue(runningId).finishedAt)
        // 其它终态/队列行不被收尸
        assertEquals(TransferRepository.STATUS_QUEUED, rows.getValue(queuedId).status)
        assertEquals(TransferRepository.STATUS_DONE, rows.getValue(doneId).status)
        assertEquals(TransferRepository.STATUS_FAILED, rows.getValue(failedId).status)
        assertNull(rows.getValue(failedId).finishedAt)
    }

    @Test
    fun clear_removesEverything() = runBlocking {
        dao.insert(record(fileName = "a.jpg"))
        dao.insert(record(fileName = "b.jpg", startedAt = 2_000L))

        dao.clear()

        assertTrue(dao.observeRecent().first().isEmpty())
        assertTrue(dao.queued().isEmpty())
        assertNull(dao.findResumable("a.jpg"))
    }

    @Test
    fun repository_isPassThrough_andConstantsMatchSqlLiterals() = runBlocking {
        // 常量与 DAO SQL 字面量（direction 0/1、status 0/1/2/3/4）互相锁定
        assertEquals(0, TransferRepository.DIR_RECV)
        assertEquals(1, TransferRepository.DIR_SEND)
        assertEquals(0, TransferRepository.STATUS_RUNNING)
        assertEquals(1, TransferRepository.STATUS_DONE)
        assertEquals(2, TransferRepository.STATUS_FAILED)
        assertEquals(3, TransferRepository.STATUS_CANCELLED)
        assertEquals(4, TransferRepository.STATUS_QUEUED)

        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val repo = TransferRepository(dao, context)
        val id = repo.enqueue(record(fileName = "via-repo.jpg", status = TransferRepository.STATUS_QUEUED, direction = TransferRepository.DIR_SEND))

        repo.updateProgress(id, 7L)
        val queuedForPc = repo.queuedForPc()
        assertEquals(1, queuedForPc.size)
        assertEquals(7L, queuedForPc.single().transferredBytes)

        val resumableId = repo.enqueue(record(fileName = "half.bin", status = TransferRepository.STATUS_RUNNING))
        assertEquals(resumableId, repo.findResumable("half.bin")?.id)

        repo.failAllRunning(1_234L)
        assertEquals(TransferRepository.STATUS_FAILED, repo.findResumable("half.bin")?.status)

        repo.finish(id, TransferRepository.STATUS_DONE, null, 5_000L, 100L)
        assertEquals(TransferRepository.STATUS_DONE, dao.observeRecent().first().first { it.id == id }.status)

        repo.clear()
        assertTrue(repo.observeRecent().first().isEmpty())
    }
}

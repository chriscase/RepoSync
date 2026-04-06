#!/bin/bash
sqlite3 /opt/reposync/gitsvnsync.db "DELETE FROM sync_records WHERE repo_id IS NULL OR repo_id = ''"
echo "Cleaned orphaned sync records"
sqlite3 /opt/reposync/gitsvnsync.db "SELECT COUNT(*) || ' sync records remaining' FROM sync_records"
sqlite3 /opt/reposync/gitsvnsync.db "SELECT COUNT(*) || ' with repo_id' FROM sync_records WHERE repo_id IS NOT NULL AND repo_id != ''"

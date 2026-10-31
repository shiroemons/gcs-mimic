<?php
declare(strict_types=1);

namespace GcsMimic\Migration;

use Google\Cloud\Storage\Bucket;

/** Lifecycle::clearRules() emits []; keep the JSON API's lifecycle object instead. */
function clearLifecycleRules(Bucket $bucket): void
{
    $bucket->update(['lifecycle' => ['rule' => []]]);
}

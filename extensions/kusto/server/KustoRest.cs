using System.Diagnostics;
using System.Net.Http.Headers;
using System.Text;
using System.Text.Json;

/// <summary>
/// Runs management commands against a Kusto cluster over the REST API, signing in with the Azure CLI.
/// Only what schema loading needs: the language server does not run queries.
/// </summary>
internal sealed class KustoRestClient
{
    private static readonly HttpClient Http = new() { Timeout = TimeSpan.FromSeconds(60) };
    private static readonly TimeSpan TokenLifetime = TimeSpan.FromMinutes(30);

    // Tests point cluster names at a local fake service and skip the Azure CLI.
    private readonly Dictionary<string, string> testEndpoints = ReadTestEndpoints();
    private readonly string? testToken = Environment.GetEnvironmentVariable("KUSTO_LSP_TEST_TOKEN");
    private readonly Dictionary<string, (string Token, DateTime FetchedAt)> tokens = new();
    private readonly Dictionary<string, Task<string?>> resources = new();
    private readonly SemaphoreSlim tokenLock = new(1, 1);

    /// <summary>
    /// The host a name refers to: `help` and `help.kusto.windows.net` are the same cluster, and a
    /// host in another cloud keeps its own domain.
    /// </summary>
    public static string ClusterHost(string name)
    {
        var host = name.Trim();
        var scheme = host.IndexOf("://", StringComparison.Ordinal);
        if (scheme >= 0)
            host = host[(scheme + 3)..];
        host = host.TrimEnd('/').ToLowerInvariant();
        return host.Contains(".kusto.", StringComparison.Ordinal) ? host : host + ".kusto.windows.net";
    }

    public async Task<List<Dictionary<string, JsonElement>>> ExecuteManagementAsync(
        string host,
        string database,
        string command,
        CancellationToken cancellationToken)
    {
        var baseUrl = BaseUrl(host);
        var token = await GetTokenAsync(host, baseUrl, cancellationToken);

        var (status, body) = await SendManagementAsync(baseUrl, token, database, command, cancellationToken);
        if (status is < 200 or >= 300)
            throw new InvalidOperationException($"{host}: {ErrorMessage(status, body)}");

        using var document = JsonDocument.Parse(body);
        var rows = new List<Dictionary<string, JsonElement>>();
        var table = document.RootElement.GetProperty("Tables")[0];
        var columns = table.GetProperty("Columns").EnumerateArray()
            .Select(column => column.GetProperty("ColumnName").GetString() ?? "")
            .ToArray();
        foreach (var row in table.GetProperty("Rows").EnumerateArray())
        {
            var values = new Dictionary<string, JsonElement>(StringComparer.Ordinal);
            var index = 0;
            foreach (var cell in row.EnumerateArray())
                values[columns[index++]] = cell.Clone();
            rows.Add(values);
        }
        return rows;
    }

    /// <summary>
    /// Management commands here only read, so a request that fails to be sent is sent once more: a
    /// connection kept from an earlier request may have been closed by the other end.
    /// </summary>
    private static async Task<(int Status, string Body)> SendManagementAsync(
        string baseUrl,
        string token,
        string database,
        string command,
        CancellationToken cancellationToken)
    {
        for (var attempt = 1; ; attempt++)
        {
            using var request = new HttpRequestMessage(HttpMethod.Post, baseUrl + "/v1/rest/mgmt")
            {
                Content = new StringContent(
                    JsonSerializer.Serialize(new { db = database, csl = command }),
                    Encoding.UTF8,
                    "application/json")
            };
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", token);
            request.Headers.Accept.Add(new MediaTypeWithQualityHeaderValue("application/json"));
            request.Headers.Add("x-ms-client-request-id", $"ZedKustoLsp;{Guid.NewGuid()}");

            try
            {
                using var response = await Http.SendAsync(request, cancellationToken);
                return ((int)response.StatusCode, await response.Content.ReadAsStringAsync(cancellationToken));
            }
            catch (HttpRequestException) when (attempt == 1)
            {
            }
        }
    }

    private string BaseUrl(string host) =>
        testEndpoints.TryGetValue(host, out var endpoint) ? endpoint.TrimEnd('/') : "https://" + host;

    private async Task<string> GetTokenAsync(string host, string baseUrl, CancellationToken cancellationToken)
    {
        if (testToken is not null)
            return testToken;

        var resource = await GetResourceAsync(baseUrl, cancellationToken);
        await tokenLock.WaitAsync(cancellationToken);
        try
        {
            if (tokens.TryGetValue(resource, out var cached) && DateTime.UtcNow - cached.FetchedAt < TokenLifetime)
                return cached.Token;

            var token = await RunAzureCliAsync(resource, cancellationToken);
            tokens[resource] = (token, DateTime.UtcNow);
            return token;
        }
        finally
        {
            tokenLock.Release();
        }
    }

    /// <summary>
    /// The audience the service wants in a token; every public cluster says kusto.kusto.windows.net.
    /// It does not change, so it is asked for once per cluster, and requests that arrive while it
    /// is being asked for wait for that answer. A failed answer is not remembered.
    /// </summary>
    private async Task<string> GetResourceAsync(string baseUrl, CancellationToken cancellationToken)
    {
        Task<string?> pending;
        lock (resources)
        {
            if (!resources.TryGetValue(baseUrl, out pending!))
                resources[baseUrl] = pending = FetchResourceAsync(baseUrl);
        }

        var resource = await pending.WaitAsync(cancellationToken);
        if (resource is not null)
            return resource;

        lock (resources)
        {
            if (resources.TryGetValue(baseUrl, out var current) && current == pending)
                resources.Remove(baseUrl);
        }
        return baseUrl;
    }

    private static async Task<string?> FetchResourceAsync(string baseUrl)
    {
        try
        {
            var body = await Http.GetStringAsync(baseUrl + "/v1/rest/auth/metadata");
            using var document = JsonDocument.Parse(body);
            var resource = document.RootElement.GetProperty("AzureAD").GetProperty("KustoServiceResourceId").GetString();
            if (!string.IsNullOrEmpty(resource))
                return resource;
        }
        catch (Exception exception) when (exception is HttpRequestException or JsonException or KeyNotFoundException)
        {
            Console.Error.WriteLine($"Could not read the token audience of {baseUrl}: {exception.Message}");
        }
        return null;
    }

    private static async Task<string> RunAzureCliAsync(string resource, CancellationToken cancellationToken)
    {
        var start = new ProcessStartInfo("az")
        {
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            RedirectStandardInput = true
        };
        foreach (var argument in new[] { "account", "get-access-token", "--resource", resource, "--output", "json" })
            start.ArgumentList.Add(argument);

        Process process;
        try
        {
            process = Process.Start(start) ?? throw new InvalidOperationException("could not start the Azure CLI");
        }
        catch (System.ComponentModel.Win32Exception)
        {
            throw new InvalidOperationException("The Azure CLI (az) was not found. Install it, then run `az login`.");
        }

        using (process)
        {
            process.StandardInput.Close();
            var output = process.StandardOutput.ReadToEndAsync(cancellationToken);
            var error = process.StandardError.ReadToEndAsync(cancellationToken);
            await process.WaitForExitAsync(cancellationToken);
            if (process.ExitCode != 0)
                throw new InvalidOperationException($"The Azure CLI could not get a token: {(await error).Trim()}");

            using var document = JsonDocument.Parse(await output);
            return document.RootElement.GetProperty("accessToken").GetString()
                ?? throw new InvalidOperationException("The Azure CLI returned no token.");
        }
    }

    private static string ErrorMessage(int status, string body)
    {
        try
        {
            using var document = JsonDocument.Parse(body);
            if (document.RootElement.TryGetProperty("error", out var error))
            {
                var deepest = error;
                while (deepest.TryGetProperty("innererror", out var inner) && inner.ValueKind == JsonValueKind.Object)
                    deepest = inner;
                foreach (var candidate in new[] { deepest, error })
                    foreach (var key in new[] { "@message", "message" })
                        if (candidate.TryGetProperty(key, out var text) && !string.IsNullOrEmpty(text.GetString()))
                            return text.GetString()!;
            }
        }
        catch (JsonException)
        {
        }
        return $"HTTP {status}";
    }

    private static Dictionary<string, string> ReadTestEndpoints()
    {
        var text = Environment.GetEnvironmentVariable("KUSTO_LSP_TEST_ENDPOINTS");
        return string.IsNullOrEmpty(text)
            ? new Dictionary<string, string>()
            : JsonSerializer.Deserialize<Dictionary<string, string>>(text) ?? new Dictionary<string, string>();
    }
}

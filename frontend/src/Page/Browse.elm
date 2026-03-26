module Page.Browse exposing (Model, Msg, currentPath, init, update, updateParams, view)

import Api exposing (DirListing, Entry(..))
import Browser.Navigation as Nav
import Html exposing (..)
import Html.Attributes exposing (..)
import Html.Events exposing (onClick, onInput, onSubmit)
import Http
import Route


pageSize : Int
pageSize =
    48


type Model
    = Loading Nav.Key Route.BrowseParams
    | Loaded BrowseState
    | Failed String


type alias BrowseState =
    { key : Nav.Key
    , listing : DirListing
    , query : String
    , page : Int
    }


type Msg
    = GotListing (Result Http.Error DirListing)
    | SearchInput String
    | SearchCommit
    | GoToPage Int


init : Nav.Key -> Route.BrowseParams -> ( Model, Cmd Msg )
init key params =
    ( Loading key params, Api.getBrowse params.path GotListing )


currentPath : Model -> String
currentPath model =
    case model of
        Loading _ params ->
            params.path

        Loaded state ->
            state.listing.path

        Failed _ ->
            ""


{-| Update query and page from a new URL without re-fetching the listing.
Called by Main when the same directory is navigated to with different params.
-}
updateParams : Route.BrowseParams -> Model -> ( Model, Cmd Msg )
updateParams params model =
    case model of
        Loading key existingParams ->
            ( Loading key { existingParams | query = params.query, page = params.page }
            , Cmd.none
            )

        Loaded state ->
            ( Loaded { state | query = params.query, page = params.page }
            , Cmd.none
            )

        Failed _ ->
            ( model, Cmd.none )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotListing (Ok listing) ->
            case model of
                Loading key params ->
                    ( Loaded
                        { key = key
                        , listing = listing
                        , query = params.query
                        , page = params.page
                        }
                    , Cmd.none
                    )

                _ ->
                    ( model, Cmd.none )

        GotListing (Err err) ->
            ( Failed (httpErrorToString err), Cmd.none )

        SearchInput q ->
            case model of
                Loaded state ->
                    let
                        newState =
                            { state | query = q, page = 1 }

                        url =
                            Route.toString
                                (Route.Browse
                                    { path = state.listing.path
                                    , query = q
                                    , page = 1
                                    }
                                )
                    in
                    ( Loaded newState, Nav.replaceUrl state.key url )

                _ ->
                    ( model, Cmd.none )

        SearchCommit ->
            case model of
                Loaded state ->
                    ( model
                    , Nav.pushUrl state.key
                        (Route.toString
                            (Route.Browse
                                { path = state.listing.path
                                , query = state.query
                                , page = state.page
                                }
                            )
                        )
                    )

                _ ->
                    ( model, Cmd.none )

        GoToPage p ->
            case model of
                Loaded state ->
                    let
                        newState =
                            { state | page = p }

                        url =
                            Route.toString
                                (Route.Browse
                                    { path = state.listing.path
                                    , query = state.query
                                    , page = p
                                    }
                                )
                    in
                    ( Loaded newState, Nav.pushUrl state.key url )

                _ ->
                    ( model, Cmd.none )


view : Model -> Html Msg
view model =
    case model of
        Loading _ params ->
            p [ style "padding" "1rem" ] [ text ("Loading " ++ params.path ++ "…") ]

        Failed err ->
            p [ style "padding" "1rem", style "color" "var(--color-error)" ]
                [ text ("Error: " ++ err) ]

        Loaded state ->
            let
                allDirs =
                    state.listing.entries |> List.filter isDirectory

                filteredVids =
                    state.listing.entries |> List.filter (matchesQuery state.query)

                totalVideos =
                    List.length filteredVids

                totalPages =
                    Basics.max 1 ((totalVideos + pageSize - 1) // pageSize)

                safePage =
                    Basics.min state.page totalPages

                pageVids =
                    filteredVids
                        |> List.drop ((safePage - 1) * pageSize)
                        |> List.take pageSize

                noResults =
                    List.isEmpty filteredVids && not (String.isEmpty state.query)
            in
            div []
                [ viewBreadcrumb state.listing.path
                , viewSearchBar state.query
                , div [ style "padding" "0.5rem 1rem" ]
                    (List.map viewEntry allDirs
                        ++ (if noResults then
                                [ p
                                    [ style "padding" "1rem 0"
                                    , style "color" "var(--color-text)"
                                    ]
                                    [ text ("No results for \"" ++ state.query ++ "\".") ]
                                ]

                            else
                                List.map viewEntry pageVids
                           )
                    )
                , if totalPages > 1 then
                    viewPagination safePage totalPages

                  else
                    text ""
                ]


isDirectory : Entry -> Bool
isDirectory entry =
    case entry of
        Directory _ ->
            True

        _ ->
            False


matchesQuery : String -> Entry -> Bool
matchesQuery query entry =
    case entry of
        Video v ->
            if String.isEmpty query then
                True

            else
                let
                    q =
                        String.toLower query

                    contains field =
                        String.contains q (String.toLower field)

                    inField =
                        Maybe.map contains >> Maybe.withDefault False
                in
                contains v.name
                    || inField v.title
                    || inField v.description
                    || inField v.channel

        Directory _ ->
            False


viewSearchBar : String -> Html Msg
viewSearchBar query =
    Html.form
        [ onSubmit SearchCommit
        , style "padding" "0.5rem 1rem"
        , style "display" "flex"
        , style "gap" "0.5rem"
        ]
        [ input
            [ type_ "search"
            , placeholder "Search…"
            , value query
            , onInput SearchInput
            , style "flex" "1"
            , style "padding" "0.4rem 0.6rem"
            , style "font-size" "1rem"
            , style "border" "1px solid var(--color-surface)"
            , style "background" "var(--color-bg)"
            , style "color" "var(--color-text)"
            , style "border-radius" "4px"
            ]
            []
        , button
            [ type_ "submit"
            , style "padding" "0.4rem 0.8rem"
            , style "font-size" "1rem"
            , style "cursor" "pointer"
            ]
            [ text "Search" ]
        ]


viewBreadcrumb : String -> Html Msg
viewBreadcrumb path =
    let
        parts =
            path |> String.split "/" |> List.filter (not << String.isEmpty)

        crumbs =
            List.indexedMap
                (\i part ->
                    let
                        crumbPath =
                            parts |> List.take (i + 1) |> String.join "/"
                    in
                    span []
                        [ text " / "
                        , a
                            [ href
                                (Route.toString
                                    (Route.Browse
                                        { path = crumbPath, query = "", page = 1 }
                                    )
                                )
                            ]
                            [ text part ]
                        ]
                )
                parts
    in
    nav [ style "padding" "0.75rem 1rem", style "background" "var(--color-surface)" ]
        (a
            [ href (Route.toString (Route.Browse { path = "", query = "", page = 1 })) ]
            [ text "Home" ]
            :: crumbs
        )


viewEntry : Entry -> Html Msg
viewEntry entry =
    case entry of
        Directory { name, path } ->
            div [ style "padding" "0.5rem 0" ]
                [ a
                    [ href
                        (Route.toString
                            (Route.Browse { path = path, query = "", page = 1 })
                        )
                    ]
                    [ text ("📁 " ++ name) ]
                ]

        Video { name, path, thumbPath, title } ->
            div [ style "display" "inline-block", style "margin" "0.5rem" ]
                [ a [ href (Route.toString (Route.Player path)) ]
                    [ case thumbPath of
                        Just tp ->
                            img
                                [ src (Api.thumbUrl tp)
                                , alt name
                                , style "width" "200px"
                                , style "height" "112px"
                                , style "object-fit" "cover"
                                , style "display" "block"
                                ]
                                []

                        Nothing ->
                            div
                                [ style "width" "200px"
                                , style "height" "112px"
                                , style "background" "var(--color-placeholder)"
                                , style "display" "flex"
                                , style "align-items" "center"
                                , style "justify-content" "center"
                                ]
                                [ text "▶" ]
                    , div
                        [ style "max-width" "200px"
                        , style "overflow" "hidden"
                        , style "text-overflow" "ellipsis"
                        , style "white-space" "nowrap"
                        , style "font-size" "0.85rem"
                        , style "margin-top" "0.25rem"
                        ]
                        [ text (Maybe.withDefault name title) ]
                    ]
                ]


type PageItem
    = Page Int
    | Gap


{-| Build a list of page items with gaps where pages are skipped.
Always includes the first and last page, and pages within 2 of current.
-}
pageItems : Int -> Int -> List PageItem
pageItems current total =
    let
        shouldInclude i =
            i == 1 || i == total || abs (i - current) <= 2

        build i lastIncluded acc =
            if i > total then
                List.reverse acc

            else if shouldInclude i then
                let
                    gapped =
                        if lastIncluded >= 0 && i > lastIncluded + 1 then
                            Gap :: acc

                        else
                            acc
                in
                build (i + 1) i (Page i :: gapped)

            else
                build (i + 1) lastIncluded acc
    in
    build 1 -1 []


viewPagination : Int -> Int -> Html Msg
viewPagination current total =
    div
        [ style "padding" "1rem"
        , style "text-align" "center"
        ]
        (List.map (viewPageItem current) (pageItems current total))


viewPageItem : Int -> PageItem -> Html Msg
viewPageItem current item =
    case item of
        Gap ->
            span
                [ style "padding" "0.3rem 0.4rem"
                , style "display" "inline-block"
                ]
                [ text "…" ]

        Page p ->
            if p == current then
                span
                    [ style "padding" "0.3rem 0.6rem"
                    , style "margin" "0 0.1rem"
                    , style "font-weight" "bold"
                    , style "display" "inline-block"
                    ]
                    [ text (String.fromInt p) ]

            else
                button
                    [ onClick (GoToPage p)
                    , style "padding" "0.3rem 0.6rem"
                    , style "margin" "0 0.1rem"
                    , style "cursor" "pointer"
                    ]
                    [ text (String.fromInt p) ]


httpErrorToString : Http.Error -> String
httpErrorToString err =
    case err of
        Http.BadUrl url ->
            "Bad URL: " ++ url

        Http.Timeout ->
            "Request timed out"

        Http.NetworkError ->
            "Network error"

        Http.BadStatus status ->
            "Server error: " ++ String.fromInt status

        Http.BadBody body ->
            "Unexpected response: " ++ body
